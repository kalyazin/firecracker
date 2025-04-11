// Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs.

mod uffd_utils;

use std::{os::fd::AsRawFd, time::Instant};
use std::os::unix::net::UnixListener;
use std::fs::File;

use libc::pwrite64;
use uffd_utils::{uffd_continue, Runtime, UffdHandler};
use utils::ioctl::ioctl_with_ref;
use utils::syscall::SyscallReturnCode;
use vmm::vstate::guest_memfd::{
    kvm_guest_memfd_copy, kvm_memory_attributes, KVM_GUEST_MEMFD_COPY,
    KVM_MEMORY_ATTRIBUTE_PRIVATE, KVM_SET_MEMORY_ATTRIBUTES,
};

fn main() {
    let mut args = std::env::args();
    let uffd_sock_path = args.nth(1).expect("No socket path given");
    let mem_file_path = args.next().expect("No memory file given");

    let file = File::open(mem_file_path).expect("Cannot open memfile");

    // Get Uffd from UDS. We'll use the uffd to handle PFs for Firecracker.
    let listener = UnixListener::bind(uffd_sock_path).expect("Cannot bind to socket path");
    let (stream, _) = listener.accept().expect("Cannot listen on UDS socket");

    stream
        .set_nonblocking(true)
        .expect("Failed to set non-blocking mode");

    let mut runtime = Runtime::new(stream, file);
    runtime.run(
        |uffd_handler: &mut UffdHandler, ret_gpa: &mut u64, ret_len: &mut u64| {
            // Read an event from the userfaultfd.
            let event = uffd_handler
                .read_event_uffd()
                .expect("Failed to read uffd_msg")
                .expect("uffd_msg not ready");

            let useraddr = event;
            let region_base = uffd_handler.mem_regions[0].mapping.base_host_virt_addr;

            let gpa = useraddr - region_base;
            let gfn = gpa / 4096;

            let sizes: Vec<usize> = uffd_handler
                .mem_regions
                .iter()
                .map(|region| region.mapping.size as usize)
                .collect();
            let mem_size = sizes[0];

            *ret_gpa = 4096 * gfn;
            *ret_len = 4096;

            let src = uffd_handler.backing_buffer as u64 + *ret_gpa;
            let dst_cont = region_base as u64 + *ret_gpa;
            let dst = uffd_handler.guest_memfd_addr as u64 + *ret_gpa;

            let start_time = Instant::now();
            println!("event (uffd) {gfn}");
            if uffd_handler.bitmap.get(gfn as usize).unwrap() == false {
                println!("writing (uffd) {gfn}...");
                unsafe {
                    std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, *ret_len as _);
                }
                let elapsed_time = start_time.elapsed();
                println!("copied in {:?}", elapsed_time);
            }

            let ret = unsafe {
                uffd_continue(uffd_handler.uffd.as_raw_fd(), dst_cont as _, *ret_len)
            };
            ret.unwrap();

            uffd_handler.bitmap.set(gfn as _, true);
        },
        |uffd_handler: &mut UffdHandler, gfn: u64, ret_gpa: &mut u64, ret_len: &mut u64| {
            let sizes: Vec<usize> = uffd_handler
                .mem_regions
                .iter()
                .map(|region| region.mapping.size as usize)
                .collect();
            let mem_size = sizes[0];

            *ret_gpa = 0;
            *ret_len = mem_size as u64;

            let src = uffd_handler.backing_buffer as u64;
            println!("gfn (stage-2) {gfn}");
            let start_time = Instant::now();

            const PAGE_SIZE: u64 = 4096;
            const MAX_WRITE_SIZE: u64 = 2147479552; // Maximum write size (close to 2GB)
            let total_pages = (mem_size as u64) / PAGE_SIZE;

            let mut start_idx = 0;
            while start_idx < total_pages as usize {
                // Find the start of the next range of zeros
                if let Some(zero_start) = uffd_handler.bitmap.as_bitslice()[start_idx..]
                    .iter()
                    .position(|bit| !*bit)
                {
                    start_idx += zero_start;
                    // Find the end of this range of zeros
                    let zero_range = &uffd_handler.bitmap.as_bitslice()[start_idx..];
                    let zeros_count = zero_range.iter().take_while(|bit| !**bit).count();

                    let start_gpa = (start_idx as u64) * PAGE_SIZE;
                    let total_length = (zeros_count as u64) * PAGE_SIZE;

                    // Process the range in chunks of MAX_WRITE_SIZE or less
                    let mut written = 0;
                    while written < total_length {
                        let chunk_length = std::cmp::min(MAX_WRITE_SIZE, total_length - written);
                        let current_gpa = start_gpa + written;

                        unsafe {
                            println!("copying gfn {} to {}...",
                                current_gpa / 4096,
                                (current_gpa + chunk_length - 1) / 4096
                            );

                            let bytes_written = pwrite64(
                                uffd_handler.guest_memfd.as_raw_fd(),
                                (src + current_gpa) as _,
                                chunk_length as _,
                                current_gpa as _
                            );

                            if bytes_written == -1 {
                                panic!("Failed to call write: {}", std::io::Error::last_os_error());
                            }
                            println!("Wrote {} bytes at GPA 0x{:x}", bytes_written, current_gpa);
                        }

                        written += chunk_length;
                    }

                    // Set the corresponding bits in the bitmap
                    uffd_handler.bitmap.get_mut(start_idx..start_idx + zeros_count).unwrap().fill(true);

                    // Move to the next unprocessed page
                    start_idx += zeros_count;
                } else {
                    // No more zeros found, we're done
                    break;
                }
            }

            let elapsed_time = start_time.elapsed();
            println!("copied in {:?}", elapsed_time);
        },
    );
}

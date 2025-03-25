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

            /* *ret_gpa = 4096 * gfn;
            *ret_len = 4096; */

            *ret_gpa = 0;
            *ret_len = mem_size as _;

            let src = uffd_handler.backing_buffer as u64 + *ret_gpa;
            let dst_cont = region_base as u64 + *ret_gpa;
            let dst = uffd_handler.guest_memfd_addr as u64 + *ret_gpa;

            /* println!("uffd: about to madvise all pages in gpa 0x{ret_gpa:x} len {ret_len}...");
            let start_time = Instant::now();
            unsafe {
                // Call madvise with MADV_POPULATE_WRITE
                let result = libc::madvise(
                    dst as *mut libc::c_void,
                    *ret_len as libc::size_t,
                    libc::MADV_POPULATE_WRITE
                );
                
                if result != 0 {
                    panic!("Failed to call madvise: {}", std::io::Error::last_os_error());
                }
            }
            let elapsed_time = start_time.elapsed();
            println!("madvised in {:?}", elapsed_time); */

            println!("uffd: about to memcpy all pages in gpa 0x{ret_gpa:x} len {ret_len}...");
            let start_time = Instant::now();
            /* unsafe {
                let bytes_written = pwrite64(
                    uffd_handler.guest_memfd.as_raw_fd(),
                    src as _,
                    2 * 1024 * 1024 * 1024, // *ret_len as _,
                    *ret_gpa as _
                );

                if bytes_written == -1 {
                    panic!("Failed to call write: {}", std::io::Error::last_os_error());
                }
                println!("Wrote {} bytes", bytes_written);
            }
            unsafe {
                let bytes_written = pwrite64(
                    uffd_handler.guest_memfd.as_raw_fd(),
                    (src + 2 * 1024 * 1024 * 1024) as _,
                    1 * 1024 * 1024 * 1024, // *ret_len as _,
                    2 * 1024 * 1024 * 1024 // *ret_gpa as _
                );

                if bytes_written == -1 {
                    panic!("Failed to call write: {}", std::io::Error::last_os_error());
                }
                println!("Wrote {} bytes", bytes_written);
            } */
            /* unsafe {
                let bytes_written = pwrite64(
                    uffd_handler.guest_memfd.as_raw_fd(),
                    src as _,
                    4096,
                    (gfn * 4096).try_into().unwrap(),
                );

                if bytes_written == -1 {
                    panic!("Failed to call write: {}", std::io::Error::last_os_error());
                }
                //println!("Wrote {} bytes", bytes_written);
            } */
            unsafe {
                std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, *ret_len as _);
            }
            let elapsed_time = start_time.elapsed();
            println!("copied in {:?}", elapsed_time);

            // let dst = (useraddr as usize & !(uffd_handler.page_size - 1)) as *mut libc::c_void;
            // println!("continuing (uffd) {gfn} to {dst:?}...");
            let ret = unsafe {
                uffd_continue(uffd_handler.uffd.as_raw_fd(), dst_cont as _, *ret_len)
            };
            ret.unwrap();
            // println!("continued.");
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

            let region_base = uffd_handler.mem_regions[0].mapping.base_host_virt_addr;

            println!("gfn (stage-2) {gfn}");

            let src = uffd_handler.backing_buffer as u64 + *ret_gpa;
            let dst_cont = region_base as u64 + *ret_gpa;
            let dst = uffd_handler.guest_memfd_addr as u64 + *ret_gpa;

            println!("exit: about to memcpy all pages in gpa 0x{ret_gpa:x} len {ret_len}...");
            let start_time = Instant::now();
            unsafe {
                let bytes_written = pwrite64(
                    uffd_handler.guest_memfd.as_raw_fd(),
                    src as _,
                    2 * 1024 * 1024 * 1024, // *ret_len as _,
                    *ret_gpa as _
                );

                if bytes_written == -1 {
                    panic!("Failed to call write: {}", std::io::Error::last_os_error());
                }
                println!("Wrote {} bytes", bytes_written);
            }
            unsafe {
                let bytes_written = pwrite64(
                    uffd_handler.guest_memfd.as_raw_fd(),
                    (src + 2 * 1024 * 1024 * 1024) as _,
                    1 * 1024 * 1024 * 1024, // *ret_len as _,
                    2 * 1024 * 1024 * 1024 // *ret_gpa as _
                );

                if bytes_written == -1 {
                    panic!("Failed to call write: {}", std::io::Error::last_os_error());
                }
                println!("Wrote {} bytes", bytes_written);
            }
            /* unsafe {
                std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, *ret_len as _);
            } */
            let elapsed_time = start_time.elapsed();
            println!("copied in {:?}", elapsed_time);

            // Can UFFDIO_CONTINUE, but need to exclude already processed to avoid EEXIST
            // println!("continuing (uffd) {gfn} to {dst:?}...");
            /* let ret = unsafe {
                uffd_continue(uffd_handler.uffd.as_raw_fd(), dst_cont as _, *ret_len)
            };
            ret.unwrap(); */
            // println!("continued.");
        },
    );
}

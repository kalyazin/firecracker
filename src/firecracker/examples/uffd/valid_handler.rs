// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs.

mod uffd_utils;

use std::os::unix::net::UnixListener;
use std::{fs::File, os::fd::AsRawFd};

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

            *ret_gpa = 4096 * gfn;
            *ret_len = 4096;

            let src = uffd_handler.backing_buffer as u64 + gpa;
            // println!("writing (uffd) {gfn}...");
            unsafe {
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
            }

            let dst = (useraddr as usize & !(uffd_handler.page_size - 1)) as *mut libc::c_void;
            // println!("copying (uffd) {gfn} to {dst:?}...");
            let ret = unsafe {
                /* uffd_handler.uffd.copy(uffd_handler.backing_buffer.offset(4096 * gfn as isize) as *const c_void,
                dst, 4096, true).unwrap() */
                uffd_continue(uffd_handler.uffd.as_raw_fd(), dst as _, 4096)
            };
            ret.unwrap();
            // println!("continued.");
        },
        |uffd_handler: &mut UffdHandler, gfn: u64, ret_gpa: &mut u64, ret_len: &mut u64| {
            *ret_gpa = 4096 * gfn;
            *ret_len = 4096;

            let src = uffd_handler.backing_buffer as u64 + *ret_gpa;

            // println!("copying (vmexit) {}...", gfn);
            unsafe {
                let bytes_written = pwrite64(
                    uffd_handler.guest_memfd.as_raw_fd(),
                    src as _,
                    *ret_len as _,
                    *ret_gpa as _
                );

                if bytes_written == -1 {
                    panic!("Failed to call write: {}", std::io::Error::last_os_error());
                }
                //println!("Wrote {} bytes", bytes_written);
            }
            // println!("copied");

            // Can UFFDIO_CONTINUE, but need to exclude already processed to avoid EEXIST
        },
    );
}

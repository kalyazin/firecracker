// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs.

mod uffd_utils;

use std::os::unix::net::UnixListener;
use std::{fs::File, os::fd::AsRawFd};

use libc::pwrite64;
use uffd_utils::{Runtime, UffdHandler};
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
    let apf_sock_path = args.next().expect("No apf socket path given");

    let file = File::open(mem_file_path).expect("Cannot open memfile");

    // Get Uffd from UDS. We'll use the uffd to handle PFs for Firecracker.
    let listener = UnixListener::bind(uffd_sock_path).expect("Cannot bind to socket path");
    let (stream, _) = listener.accept().expect("Cannot listen on UDS socket");

    let apf_listener = UnixListener::bind(apf_sock_path).expect("Cannot bind to apf socket path");
    let (apf_stream, _) = apf_listener
        .accept()
        .expect("Cannot listen on UDS APF socket");

    apf_stream
        .set_nonblocking(true)
        .expect("Failed to set non-blocking mode");

    let mut runtime = Runtime::new(stream, file, apf_stream);
    runtime.run(
        |uffd_handler: &mut UffdHandler| {
            // Read an event from the userfaultfd.
            let event = uffd_handler
                .read_event_uffd()
                .expect("Failed to read uffd_msg")
                .expect("uffd_msg not ready");

            let useraddr = event;
            let region_base = uffd_handler.mem_regions[0].mapping.base_host_virt_addr;

            let gpa = useraddr - region_base;
            let gfn = gpa / 4096;

            use std::os::raw::c_void;
            let dst = (useraddr as usize & !(uffd_handler.page_size - 1)) as *mut libc::c_void;
            println!("copying (uffd) {gfn} to {dst:?}...");
            let ret = unsafe {
                uffd_handler.uffd.copy(uffd_handler.backing_buffer.offset(4096 * gfn as isize) as *const c_void,
                dst, 4096, true).unwrap()
            };
            assert!(ret > 0);
            println!("continued.");
        },
        |uffd_handler: &mut UffdHandler| {
            // Read an event from the userfaultfd.
            let event = uffd_handler
                .read_event_eventfd()
                .expect("Failed to read uffd_msg")
                .expect("uffd_msg not ready");

            let gfn = event;

            println!("copying one {}...", gfn);
            use std::os::raw::c_void;
            let copy = kvm_guest_memfd_copy {
                guest_memfd: uffd_handler.guest_memfd.as_raw_fd() as _,
                from: unsafe {
                    uffd_handler.backing_buffer.offset(4096 * gfn as isize) as *const c_void
                },
                offset: 4096 * gfn,
                len: 4096,
            };
            unsafe {
                SyscallReturnCode(ioctl_with_ref(
                    &uffd_handler.kvm_fd,
                    KVM_GUEST_MEMFD_COPY(),
                    &copy,
                ))
                .into_empty_result()
                .unwrap()
            }
            println!("copied");

            // println!("about to clear uffd memattr...");
            let attributes = kvm_memory_attributes {
                address: 4096 * gfn,
                size: 4096,
                attributes: KVM_MEMORY_ATTRIBUTE_PRIVATE,
                ..Default::default()
            };

            unsafe {
                SyscallReturnCode(ioctl_with_ref(
                    &uffd_handler.kvm_fd,
                    KVM_SET_MEMORY_ATTRIBUTES(),
                    &attributes,
                ))
                .into_empty_result()
                .unwrap()
            }

            // println!("cleared.");
        },
        |uffd_handler: &mut UffdHandler, gfn: u64, ret_gpa: &mut u64, ret_len: &mut u64| {
            *ret_gpa = 4096 * gfn;
            *ret_len = 4096;

            // Populate the memory region with write-combined data
            /* use std::os::raw::c_void;
            use libc::MADV_POPULATE_WRITE;
            let result = unsafe {
                madvise(
                    uffd_handler.memfd_addr.offset(*ret_gpa as isize) as *mut c_void,
                    *ret_len as usize,
                    MADV_POPULATE_WRITE
                )
            };
            if result != 0 {
                panic!("Failed to call madvise: {}", std::io::Error::last_os_error());
            } */

            let src = uffd_handler.backing_buffer as u64 + *ret_gpa;
            /* let dst = uffd_handler.memfd_addr as u64 + *ret_gpa;

            unsafe {
                std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, *ret_len as _);
            } */

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
        },
    );
}

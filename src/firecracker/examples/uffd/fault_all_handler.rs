// Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs.

mod uffd_utils;

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::{fs::File, time::Instant};

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
            let _event = uffd_handler
                .read_event()
                .expect("Failed to read uffd_msg")
                .expect("uffd_msg not ready");

            let sizes: Vec<usize> = uffd_handler
                .mem_regions
                .iter()
                .map(|region| region.mapping.size as usize)
                .collect();
            let mem_size = sizes[0];

            println!("about to copy all pages in...");
            let start_time = Instant::now();
            use std::os::raw::c_void;
            let copy = kvm_guest_memfd_copy {
                guest_memfd: uffd_handler.guest_memfd.as_raw_fd() as _,
                from: unsafe { uffd_handler.backing_buffer.offset(0 as isize) as *const c_void },
                offset: 0,
                len: mem_size as u64,
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
            let elapsed_time = start_time.elapsed();
            println!("copied in {:?}", elapsed_time);

            println!("about to clear uffd memattr for all pages...");
            let attributes = kvm_memory_attributes {
                address: 0,
                size: mem_size as u64,
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

            println!("cleared.");
        },
        |uffd_handler: &mut UffdHandler, _gfn: u64| {
            let sizes: Vec<usize> = uffd_handler
                .mem_regions
                .iter()
                .map(|region| region.mapping.size as usize)
                .collect();
            let mem_size = sizes[0];

            println!("about to copy all pages in...");
            // FIXME: I didn't test this case
            unsafe {
                std::ptr::copy_nonoverlapping(
                    uffd_handler.backing_buffer.offset(0 as isize),
                    uffd_handler.guest_memfd_addr.offset(0 as isize),
                    mem_size,
                )
            }
            println!("copied.");
        },
    );
}

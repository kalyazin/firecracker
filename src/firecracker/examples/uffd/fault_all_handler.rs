// Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs.

mod uffd_utils;

use std::fs::File;
use std::os::unix::net::UnixListener;

use uffd_utils::{Runtime, UffdHandler};
use utils::ioctl::ioctl_with_ref;
use utils::syscall::SyscallReturnCode;
use vmm::vstate::guest_memfd::{
    kvm_memory_attributes, KVM_MEMORY_ATTRIBUTE_PRIVATE, KVM_SET_MEMORY_ATTRIBUTES,
};

fn main() {
    let mut args = std::env::args();
    let uffd_sock_path = args.nth(1).expect("No socket path given");
    let mem_file_path = args.next().expect("No memory file given");

    let file = File::open(mem_file_path).expect("Cannot open memfile");

    // Get Uffd from UDS. We'll use the uffd to handle PFs for Firecracker.
    let listener = UnixListener::bind(uffd_sock_path).expect("Cannot bind to socket path");
    let (stream, _) = listener.accept().expect("Cannot listen on UDS socket");

    let mut runtime = Runtime::new(stream, file);
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
            unsafe {
                std::ptr::copy_nonoverlapping(
                    uffd_handler.backing_buffer.offset(0 as isize),
                    uffd_handler.guest_memfd_addr.offset(0 as isize),
                    mem_size,
                )
            }
            println!("copied.");

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

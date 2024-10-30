// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs.

mod uffd_utils;

use std::os::unix::net::UnixListener;
use std::{fs::File, os::fd::AsRawFd};

use uffd_utils::{Runtime, UffdHandler};
use utils::ioctl::ioctl_with_ref;
use utils::syscall::SyscallReturnCode;
use vmm::vstate::guest_memfd::{
    kvm_guest_memfd_copy, kvm_memory_attributes, KVM_GUEST_MEMFD_COPY,
    KVM_MEMORY_ATTRIBUTE_PRIVATE, KVM_SET_MEMORY_ATTRIBUTES,
};

fn produce_excs(gfns: &[u64], wnd: u64) -> Vec<(u64, u64)> {
    gfns.iter().map(|gfn| {
        let base = gfn / wnd * wnd;
        (base, base + wnd - 1)
    }).collect()
}

fn is_in_excs(excs: &Vec<(u64, u64)>, gfn: u64) -> bool {
    excs.iter().any(|&(base, size)| {
        gfn >= base && gfn <= size
    })
}

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
                .read_event()
                .expect("Failed to read uffd_msg")
                .expect("uffd_msg not ready");

            let gfn = event;

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
            let wnd = 128;
            let excs = produce_excs(&[0xbcc1a,
                0x284f,
                0x3ca5,
                0x3ca1,
                0xdf,
                0x3ca6,
                0x3ca2,
                0x73f2,
                0x73f3,
                0x73f4,
                0x73f5,
                0x73f6,
                0x73f7,
                0x73f8,
                0x73f9,
                0x73fa,
                0x73fb,
                0x73fc
                ], wnd);

            let (offset, size) = if is_in_excs(&excs, gfn) {
                println!("in excs, win: {:x} {:x}, gfn: {:x}", gfn / wnd * wnd, gfn / wnd * wnd + wnd - 1, gfn);
                (4096 * gfn, 4096)
            } else {
                (4096 * (gfn / wnd * wnd), 4096 * wnd)
            };

            /* let offset = 4096 * (gfn / wnd * wnd);
            let size = 4096 * wnd; */

            use std::os::raw::c_void;
            let copy = kvm_guest_memfd_copy {
                guest_memfd: uffd_handler.guest_memfd.as_raw_fd() as _,
                from: unsafe {
                    uffd_handler.backing_buffer.offset(offset as isize) as *const c_void
                },
                offset: offset,
                len: size,
            };
            unsafe {
                SyscallReturnCode(ioctl_with_ref(
                    &uffd_handler.kvm_fd,
                    KVM_GUEST_MEMFD_COPY(),
                    &copy,
                ))
                .into_empty_result()
                .unwrap()
            };

            *ret_gpa = offset;
            *ret_len = size;
        },
    );
}

// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a malicious page fault handler
//! which panics when a page fault occurs.

mod uffd_utils;

use std::fs::File;
use std::os::unix::net::UnixListener;

use uffd_utils::{Runtime, UffdHandler};

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

    let mut runtime = Runtime::new(stream, file, apf_stream);
    runtime.run(
        |uffd_handler: &mut UffdHandler| {
            // Read an event from the userfaultfd.
            let _event = uffd_handler
                .read_event()
                .expect("Failed to read uffd_msg")
                .expect("uffd_msg not ready");

            // FIXME: this handler is not functional.
        },
        |_uffd_handler: &mut UffdHandler, _gfn: u64| {
            // FIXME: this handler is not functional.
        },
    );
}

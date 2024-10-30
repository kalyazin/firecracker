// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

// Not everything is used by both binaries
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::ptr;

use kvm_ioctls::VmFd;
use libc::iovec;
use serde::{Deserialize, Serialize};
use userfaultfd::{Error, Uffd};
use utils::eventfd::EventFd;
use utils::sock_ctrl_msg::ScmSocket;
use vmm::vstate::guest_memfd::read_fault;

// This is the same with the one used in src/vmm.
/// This describes the mapping between Firecracker base virtual address and offset in the
/// buffer or file backend for a guest memory region. It is used to tell an external
/// process/thread where to populate the guest memory data for this range.
///
/// E.g. Guest memory contents for a region of `size` bytes can be found in the backend
/// at `offset` bytes from the beginning, and should be copied/populated into `base_host_address`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuestRegionUffdMapping {
    /// Base host virtual address where the guest memory contents for this region
    /// should be copied/populated.
    pub base_host_virt_addr: u64,
    /// Region size.
    pub size: usize,
    /// Offset in the backend file/buffer where the region contents are.
    pub offset: u64,
    /// The configured page size for this memory region.
    pub page_size_kib: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum MemPageState {
    Uninitialized,
    FromFile,
    Removed,
    Anonymous,
}

#[derive(Debug, Clone)]
pub struct MemRegion {
    pub mapping: GuestRegionUffdMapping,
    page_states: HashMap<u64, MemPageState>,
}

#[derive(Debug)]
pub struct UffdHandler {
    pub mem_regions: Vec<MemRegion>,
    pub page_size: usize,
    pub backing_buffer: *const u8,
    // For receiving fault notifications
    eventfd: EventFd,
    // For mmapping guest memory
    pub guest_memfd: File,
    // For clearing UFFD memattr
    pub kvm_fd: VmFd,
    uffd: Uffd,
    // For copying pages in
    pub guest_memfd_addr: *mut u8,
}

pub enum FromUnixStreamResult {
    NewConn(UffdHandler),
    Gfn(u64),
}

impl UffdHandler {
    pub fn from_unix_stream(
        stream: &UnixStream,
        backing_buffer: *const u8,
        size: usize,
    ) -> FromUnixStreamResult {
        let mut message_buf = vec![0u8; 1024];
        use core::ffi::c_void;
        let mut iovecs = [iovec {
            iov_base: message_buf.as_mut_ptr() as *mut c_void,
            iov_len: message_buf.len(),
        }];
        let mut fds = [0, 0, 0];
        let (read_count, _fd_count) =
            unsafe { stream.recv_with_fds(&mut iovecs[..], &mut fds).unwrap() };
        message_buf.resize(read_count, 0);

        // FIXME: if we didn't receive any fds, it's an implicit indication that it's a pagefault
        // notification.
        if fds[0] != 0 {
            let body = String::from_utf8(message_buf).unwrap();

            let mappings = serde_json::from_str::<Vec<GuestRegionUffdMapping>>(&body)
                .expect("Cannot deserialize memory mappings.");
            let memsize: usize = mappings.iter().map(|r| r.size).sum();
            // Page size is the same for all memory regions, so just grab the first one
            let page_size = mappings.first().unwrap().page_size_kib;

            // Make sure memory size matches backing data size.
            assert_eq!(memsize, size);
            assert!(page_size.is_power_of_two());

            // This one is a dummy, not used.
            let uffd = unsafe { Uffd::from_raw_fd(1000) };

            let eventfd = unsafe { EventFd::from_raw_fd(fds[0].into_raw_fd()) };
            let guest_memfd = unsafe { File::from_raw_fd(fds[1].into_raw_fd()) };

            // We've no way to construct a real VmFd from scratch as it's in kvm-ioctls repo.
            pub struct VmFdMy {
                vm: File,
                run_size: usize,
            }

            // We don't care about run_size as we don't use it here.
            let vmfd_my = VmFdMy {
                vm: unsafe { File::from_raw_fd(fds[2].into_raw_fd()) },
                run_size: 0,
            };
            let kvm_fd = unsafe { std::mem::transmute::<VmFdMy, VmFd>(vmfd_my) };

            let mem_regions = create_mem_regions(&mappings, page_size);

            let sizes: Vec<usize> = mem_regions
                .iter()
                .map(|region| region.mapping.size as usize)
                .collect();

            // # Safety:
            // File size and fd are valid
            let ret = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    sizes[0],
                    libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    guest_memfd.as_raw_fd(),
                    0,
                )
            };
            if ret == libc::MAP_FAILED {
                panic!("mmap on guest_memfd failed");
            }

            println!("guest_memfd is mmapped.");

            FromUnixStreamResult::NewConn(Self {
                mem_regions,
                page_size,
                backing_buffer,
                eventfd,
                guest_memfd,
                kvm_fd,
                uffd,
                guest_memfd_addr: ret.cast(),
            })
        } else {
            let gpa: u64 = u64::from_be_bytes(message_buf.as_slice().try_into().unwrap());
            let gfn = gpa / 4096;
            FromUnixStreamResult::Gfn(gfn)
        }
    }

    pub fn read_event(&mut self) -> Result<Option<u64>, Error> {
        let _res = self.eventfd.read().unwrap();
        let gfn = read_fault(&self.kvm_fd);
        Ok(Some(gfn))
    }

    pub fn update_mem_state_mappings(&mut self, start: u64, end: u64, state: MemPageState) {
        for region in self.mem_regions.iter_mut() {
            for (key, value) in region.page_states.iter_mut() {
                if key >= &start && key < &end {
                    *value = state;
                }
            }
        }
    }

    pub fn serve_pf(&mut self, addr: *mut u8, len: usize) {
        // Find the start of the page that the current faulting address belongs to.
        let dst = (addr as usize & !(self.page_size - 1)) as *mut libc::c_void;
        let fault_page_addr = dst as u64;

        // Get the state of the current faulting page.
        for region in self.mem_regions.iter() {
            match region.page_states.get(&fault_page_addr) {
                // Our simple PF handler has a simple strategy:
                // There exist 4 states in which a memory page can be in:
                // 1. Uninitialized - page was never touched
                // 2. FromFile - the page is populated with content from snapshotted memory file
                // 3. Removed - MADV_DONTNEED was called due to balloon inflation
                // 4. Anonymous - page was zeroed out -> this implies that more than one page fault
                //    event was received. This can be a consequence of guest reclaiming back its
                //    memory from the host (through balloon device)
                Some(MemPageState::Uninitialized) | Some(MemPageState::FromFile) => {
                    let (start, end) = self.populate_from_file(region, fault_page_addr, len);
                    self.update_mem_state_mappings(start, end, MemPageState::FromFile);
                    return;
                }
                Some(MemPageState::Removed) | Some(MemPageState::Anonymous) => {
                    let (start, end) = self.zero_out(fault_page_addr);
                    self.update_mem_state_mappings(start, end, MemPageState::Anonymous);
                    return;
                }
                None => {}
            }
        }

        panic!(
            "Could not find addr: {:?} within guest region mappings.",
            addr
        );
    }

    fn populate_from_file(&self, region: &MemRegion, dst: u64, len: usize) -> (u64, u64) {
        let offset = dst - region.mapping.base_host_virt_addr;
        let src = self.backing_buffer as u64 + region.mapping.offset + offset;

        let ret = unsafe {
            self.uffd
                .copy(src as *const _, dst as *mut _, len, true)
                .expect("Uffd copy failed")
        };

        // Make sure the UFFD copied some bytes.
        assert!(ret > 0);

        (dst, dst + len as u64)
    }

    fn zero_out(&mut self, addr: u64) -> (u64, u64) {
        let ret = unsafe {
            self.uffd
                .zeropage(addr as *mut _, self.page_size, true)
                .expect("Uffd zeropage failed")
        };
        // Make sure the UFFD zeroed out some bytes.
        assert!(ret > 0);

        (addr, addr + self.page_size as u64)
    }
}

#[derive(Debug)]
pub struct Runtime {
    stream: UnixStream,
    apf_stream: UnixStream,
    backing_file: File,
    backing_memory: *mut u8,
    backing_memory_size: usize,
    eventfds: HashMap<i32, UffdHandler>,
}

impl Runtime {
    pub fn new(stream: UnixStream, backing_file: File, apf_stream: UnixStream) -> Self {
        // Send our own fd straight away
        let buf = b"mem src file";
        let ret = stream.send_with_fd(&buf[..], backing_file.as_raw_fd()).unwrap();
        if ret == 0 {
            panic!("send mem src file failed")
        }

        let file_meta = backing_file
            .metadata()
            .expect("can not get backing file metadata");
        let backing_memory_size = file_meta.len() as usize;
        // # Safety:
        // File size and fd are valid
        let ret = unsafe {
            libc::mmap(
                ptr::null_mut(),
                backing_memory_size,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                backing_file.as_raw_fd(),
                0,
            )
        };
        if ret == libc::MAP_FAILED {
            panic!("mmap on backing file failed");
        }

        Self {
            stream,
            apf_stream,
            backing_file,
            backing_memory: ret.cast(),
            backing_memory_size,
            eventfds: HashMap::default(),
        }
    }

    /// Polls the `UnixStream` and UFFD fds in a loop.
    /// When stream is polled, new uffd is retrieved.
    /// When uffd is polled, page fault is handled by
    /// calling `pf_event_dispatch` with corresponding
    /// uffd object passed in.
    pub fn run(
        &mut self,
        pf_event_dispatch: impl Fn(&mut UffdHandler),
        pf_exit_dispatch: impl Fn(&mut UffdHandler, u64, &mut u64, &mut u64),
    ) {
        let mut pollfds = vec![];

        // Poll the stream for incoming uffds
        pollfds.push(libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });

        // Poll the stream for incoming APF connections
        pollfds.push(libc::pollfd {
            fd: self.apf_stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });

        // We can skip polling on stream fd if
        // the connection is closed.
        let mut skip_stream: usize = 0;
        loop {
            let pollfd_ptr = pollfds[skip_stream..].as_mut_ptr();
            let pollfd_size = pollfds[skip_stream..].len() as u64;

            // # Safety:
            // Pollfds vector is valid
            let mut nready = unsafe { libc::poll(pollfd_ptr, pollfd_size, -1) };

            if nready == -1 {
                panic!("Could not poll for events!")
            }

            for i in skip_stream..pollfds.len() {
                if nready == 0 {
                    break;
                }
                if pollfds[i].revents & libc::POLLHUP != 0 {
                    panic!("sighup received!");
                }
                if pollfds[i].revents & libc::POLLIN != 0 {
                    nready -= 1;
                    if pollfds[i].fd == self.stream.as_raw_fd() {
                        // Handle new uffd from stream
                        let result = UffdHandler::from_unix_stream(
                            &self.stream,
                            self.backing_memory,
                            self.backing_memory_size,
                        );

                        match result {
                            FromUnixStreamResult::NewConn(handler) => {
                                pollfds.push(libc::pollfd {
                                    fd: handler.eventfd.as_raw_fd(),
                                    events: libc::POLLIN,
                                    revents: 0,
                                });
                                self.eventfds.insert(handler.eventfd.as_raw_fd(), handler);

                                // If connection is closed, we can skip the socket from being
                                // polled.
                                if pollfds[i].revents & (libc::POLLRDHUP | libc::POLLHUP) != 0 {
                                    skip_stream = 1;
                                }
                            }
                            FromUnixStreamResult::Gfn(gfn) => {
                                let (_, handler) = self.eventfds.iter_mut().next().unwrap();
                                let mut ret_gpa = 0u64;
                                let mut ret_len: u64 = 0u64;
                                pf_exit_dispatch(handler, gfn, &mut ret_gpa, &mut ret_len);

                                // Replying 1 as an ack. Can be anything.
                                let bytes = [(1 as u64).to_be_bytes(), ret_gpa.to_be_bytes(), ret_len.to_be_bytes()].concat();
                                let _ret = self.stream.write_all(&bytes);
                            }
                        }
                    } else if pollfds[i].fd == self.apf_stream.as_raw_fd() {
                        let (_, handler) = self.eventfds.iter_mut().next().unwrap();
                        let req: &mut [u8; 8] = &mut [0; 8];

                        loop {
                            match self.apf_stream.read_exact(req) {
                                Ok(()) => {
                                    let evt: u64 =
                                        u64::from_be_bytes(req.as_slice().try_into().unwrap());
                                    let gpa = evt & 0xffff_ffff;

                                    /* let token = (evt >> 32) & 0xffff_f7ff;
                                    let notpresent_injected = evt & (1 << (32 + 11)) != 0;
                                    let vcpu_idx = evt & 0x7ff;

                                    println!(
                                        "APF: req: gpa 0x{:x} token 0x{:x} vcpu 0x{:x}, notpr {}",
                                        gpa, token, vcpu_idx, notpresent_injected
                                    ); */

                                    let gfn = gpa / 4096;

                                    let mut ret_gpa = 0u64;
                                    let mut ret_len: u64 = 0u64;
                                    pf_exit_dispatch(handler, gfn, &mut ret_gpa, &mut ret_len);

                                    // Replying gpa as an ack.
                                    let bytes = [evt.to_be_bytes(), ret_gpa.to_be_bytes(), ret_len.to_be_bytes()].concat();
                                    self.apf_stream.write_all(&bytes).unwrap();
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                                    break;
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    break;
                                }
                                Err(e) => {
                                    // Error occurred while reading from the stream
                                    eprintln!("Error reading from stream: {}", e);
                                    break;
                                }
                            }
                        }
                    } else {
                        // Handle one of uffd page faults
                        pf_event_dispatch(self.eventfds.get_mut(&pollfds[i].fd).unwrap());
                    }
                }
            }
        }
    }
}

fn create_mem_regions(mappings: &Vec<GuestRegionUffdMapping>, page_size: usize) -> Vec<MemRegion> {
    let mut mem_regions: Vec<MemRegion> = Vec::with_capacity(mappings.len());

    for r in mappings.iter() {
        let mapping = r.clone();
        let mut addr = r.base_host_virt_addr;
        let end_addr = r.base_host_virt_addr + r.size as u64;
        let mut page_states = HashMap::new();

        while addr < end_addr {
            page_states.insert(addr, MemPageState::Uninitialized);
            addr += page_size as u64;
        }
        mem_regions.push(MemRegion {
            mapping,
            page_states,
        });
    }

    mem_regions
}

#[cfg(test)]
mod tests {
    use std::mem::MaybeUninit;
    use std::os::unix::net::UnixListener;

    use utils::tempdir::TempDir;
    use utils::tempfile::TempFile;

    use super::*;

    unsafe impl Send for Runtime {}

    #[test]
    fn test_runtime() {
        let tmp_dir = TempDir::new().unwrap();
        let dummy_socket_path = tmp_dir.as_path().join("dummy_socket");
        let dummy_socket_path_clone = dummy_socket_path.clone();

        let mut uninit_runtime = Box::new(MaybeUninit::<Runtime>::uninit());
        // We will use this pointer to bypass a bunch of Rust Safety
        // for the sake of convenience.
        let runtime_ptr = uninit_runtime.as_ptr().cast::<Runtime>();

        let runtime_thread = std::thread::spawn(move || {
            let tmp_file = TempFile::new().unwrap();
            tmp_file.as_file().set_len(0x1000).unwrap();
            let dummy_mem_path = tmp_file.as_path();

            let file = File::open(dummy_mem_path).expect("Cannot open memfile");
            let listener =
                UnixListener::bind(dummy_socket_path).expect("Cannot bind to socket path");
            let (stream, _) = listener.accept().expect("Cannot listen on UDS socket");
            // Update runtime with actual runtime
            let runtime = uninit_runtime.write(Runtime::new(stream, file));
            runtime.run(|_: &mut UffdHandler| {});
        });

        // wait for runtime thread to initialize itself
        std::thread::sleep(std::time::Duration::from_millis(100));

        let stream =
            UnixStream::connect(dummy_socket_path_clone).expect("Cannot connect to the socket");

        let dummy_memory_region = vec![GuestRegionUffdMapping {
            base_host_virt_addr: 0,
            size: 0x1000,
            offset: 0,
            page_size_kib: 4096,
        }];
        let dummy_memory_region_json = serde_json::to_string(&dummy_memory_region).unwrap();

        let dummy_file_1 = TempFile::new().unwrap();
        let dummy_fd_1 = dummy_file_1.as_file().as_raw_fd();
        stream
            .send_with_fd(dummy_memory_region_json.as_bytes(), dummy_fd_1)
            .unwrap();
        // wait for the runtime thread to process message
        std::thread::sleep(std::time::Duration::from_millis(100));
        unsafe {
            assert_eq!((*runtime_ptr).eventfds.len(), 1);
        }

        let dummy_file_2 = TempFile::new().unwrap();
        let dummy_fd_2 = dummy_file_2.as_file().as_raw_fd();
        stream
            .send_with_fd(dummy_memory_region_json.as_bytes(), dummy_fd_2)
            .unwrap();
        // wait for the runtime thread to process message
        std::thread::sleep(std::time::Duration::from_millis(100));
        unsafe {
            assert_eq!((*runtime_ptr).eventfds.len(), 2);
        }

        // there is no way to properly stop runtime, so
        // we send a message with an incorrect memory region
        // to cause runtime thread to panic
        let error_memory_region = vec![GuestRegionUffdMapping {
            base_host_virt_addr: 0,
            size: 0,
            offset: 0,
            page_size_kib: 4096,
        }];
        let error_memory_region_json = serde_json::to_string(&error_memory_region).unwrap();
        stream
            .send_with_fd(error_memory_region_json.as_bytes(), dummy_fd_2)
            .unwrap();

        runtime_thread.join().unwrap_err();
    }
}

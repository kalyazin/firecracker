# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0

"""Utilities for vhost-user-blk backend."""

import os
import subprocess
import time

from framework import utils

CROSVM_CTR_SOCKET = "/crosvm_ctr.socket"


def spawn_vhost_user_backend(vm, host_mem_path, socket_path, readonly=False, backend="qemu"):
    """Spawn vhost-user-blk backend."""

    uid = vm.jailer.uid
    gid = vm.jailer.gid

    sp = f"{vm.chroot()}{socket_path}"

    if backend == "qemu":
        args = ["vhost-user-blk", "--socket-path", sp, "--blk-file", host_mem_path]
        if readonly:
            args.append("-r")
    elif backend == "crosvm":
        ro = "ro" if readonly else ""
        args = [
            "crosvm",
            "devices",
            "--disable-sandbox",
            "--control-socket",
            CROSVM_CTR_SOCKET,
            "--block",
            f"vhost={sp},path={host_mem_path},{ro}",
        ]
    else:
        assert False, f"unknown vhost-user-blk backend `{backend}`"
    proc = subprocess.Popen(args)

    # Give the backend time to initialise.
    time.sleep(1)

    assert proc is not None and proc.poll() is None, "backend is not up"

    with utils.chroot(vm.chroot()):
        # The backend will create the socket path with root rights.
        # Change rights to the jailer's.
        os.chown(socket_path, uid, gid)

    return proc


def resize(new_size, backend="crosvm"):
    """Resize vhost-user-blk drive and send config change notification"""

    if backend != "crosvm":
        assert False, f"backend `{backend}` does not support resizing"

    utils.run_cmd(f"crosvm disk resize 0 {new_size} {CROSVM_CTR_SOCKET}")

# Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Ensure multiple microVMs work correctly when spawned simultaneously."""

import asyncio
import asyncssh
from nsenter import Namespace
import socket
import time
import pytest
import shutil
import os
from pathlib import Path
import numpy as np

from framework import decorators
from framework.s3fetcher import MicrovmImageS3Fetcher
from framework.artifacts import NetIfaceConfig
from framework.artifacts import SnapshotMemBackendType
from framework.utils import run_cmd
from conftest import _test_images_s3_bucket
from integration_tests.functional.test_uffd import spawn_pf_handler, SOCKET_PATH

import host_tools.network as net_tools


NO_OF_MICROVMS = 48
NO_OF_RUNS = 100
# NO_OF_FIB_PREWARM = 10
NO_OF_FIB_PREWARM = 5 # java value
# NO_OF_FIB_RUN = 43 # about 165 sec (python)
# NO_OF_FIB_RUN = 34 # about 2.2 sec (python)
NO_OF_FIB_RUN = 32 # java value

mem_fname = "mem"
vmstate_fname = "vmstate"
shared_dir_name = "my_snapshot"


def set_up_event_loop():
    try:
        loop = asyncio.get_event_loop()
    except RuntimeError:
        # Create event loop when one is not available
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

    return loop

async def configure_and_run(microvm, network_info, run, orig_ips):
    """Auxiliary function for configuring and running microVM."""
    microvm.spawn(create_logger=False)

    # Machine configuration specified in the SLA.
    if (run):
        config = {"vcpu_count": 2, "mem_size_mib": 512}
        microvm.basic_config(**config)

        _tap, _host_ip, _guest_ip = microvm.ssh_network_config(
            network_info["config"], network_info["iface_id"],
            tapname="tap0"
        )
    else:
        iface = NetIfaceConfig()
        _tap = microvm.create_tap_and_ssh_config(
            host_ip=orig_ips['host_ip'],
            guest_ip=orig_ips['guest_ip'],
            netmask_len=iface.netmask,
            tapname=iface.tap_name,
        )

    if (run):
        microvm.start()

    if (run):
        return _tap, _host_ip, _guest_ip
    else:
        return _tap, None, None

async def connect(username, identity, sock):
    return await asyncssh.connect(
        username=username, known_hosts=None, connect_timeout=1, client_keys=[identity], sock=sock
    )

async def execute_command(conn, cmd):
    result = await conn.run(cmd)
    return result

async def push_file(conn, src, dst):
    await asyncssh.scp(src, (conn, dst))

def configure_microvms(microvms, loop, network_config, run, orig_ips):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        microvm = microvms[i]
        cmds.append(configure_and_run(microvm, {"config": network_config, "iface_id": str(i)}, run=run, orig_ips=orig_ips))
        # print(f"netns_file_path: {microvm.ssh_config['netns_file_path']}")

    start = time.time()
    results = loop.run_until_complete(asyncio.gather(*cmds))
    end = time.time()
    print(f"time config: {end - start}")

    pid_proc = []
    for i in range(NO_OF_MICROVMS):
        microvm = microvms[i]
        # print(f"jailer_clone_pid {microvm.jailer_clone_pid}")
        pid_proc.append((microvm.jailer_clone_pid, None))

    return pid_proc

def connect_to_microvms(microvms, uvm_data, loop):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        microvm = microvms[i]

        ssh_conn = net_tools.SSHConnection(microvm.ssh_config)

        with Namespace(microvm.ssh_config['netns_file_path'], "net"):
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.connect((microvm.ssh_config['hostname'], 22));
        cmds.append(connect(
            microvm.ssh_config['username'],
            microvm.ssh_config['ssh_key_path'],
            sock
        ))

        uvm_data.append({
            "sock": sock,
        })

    start = time.time()
    results = loop.run_until_complete(asyncio.gather(*cmds))
    for i in range(NO_OF_MICROVMS):
        uvm_data[i]["assh_conn"] = results[i]
    end = time.time()
    print(f"time connect: {end - start}")

def push_bin_to_microvms(uvm_data, loop):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        cmds.append(push_file(uvm_data[i]["assh_conn"], "../resources/tests/fib.py", "./fib.py"))

    start = time.time()
    _ = loop.run_until_complete(asyncio.gather(*cmds))
    end = time.time()
    print(f"time push: {end - start}")

def run_bin_on_microvms_dbg(uvm_data, loop):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        cmds.append(execute_command(uvm_data[i]["assh_conn"], "ps"))

    results = loop.run_until_complete(asyncio.gather(*cmds))
    for r in results:
        print(r.stdout)

def run_bin_on_microvms(uvm_data, loop, arg, log):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        cmds.append(execute_command(uvm_data[i]["assh_conn"], f"java -XX:+UseSerialGC Main {arg}"))
        """ cmds.append(execute_command(uvm_data[i]["assh_conn"], f"python ./fib.py {arg}")) """

    start = time.time()
    results = loop.run_until_complete(asyncio.gather(*cmds))
    """ for r in results:
        print(r.stdout) """
    end = time.time()
    print(f"time {log}: {end - start}")
    return end - start

@pytest.mark.timeout(3 * 60 * 60)
@decorators.test_context("api", NO_OF_MICROVMS)
#@pytest.mark.skipif(True, reason="debug")
def test_run_concurrency_zip(test_multiple_microvms, network_config):
    """
    Check we can spawn multiple microvms.

    @type: functional
    """
    microvms = test_multiple_microvms
    uvm_data = []

    loop = set_up_event_loop()

    pid_proc = configure_microvms(microvms, loop, network_config, run=True, orig_ips=None)
    connect_to_microvms(microvms, uvm_data, loop)
    # run_bin_on_microvms_dbg(uvm_data, loop)
    push_bin_to_microvms(uvm_data, loop)
    run_bin_on_microvms(uvm_data, loop, NO_OF_FIB_PREWARM, "prewarm")
    """ stats = capture_pid_stats_before(pid_proc) """
    run_stats(uvm_data, loop, NO_OF_RUNS)
    """ run_pid_stats(stats) """

def create_snapshot(microvm, network_config):
    loop = set_up_event_loop()

    vm_for_snapshot = microvm
    MicrovmImageS3Fetcher(_test_images_s3_bucket()).init_vm_resources("ubuntu", vm_for_snapshot)
    result = loop.run_until_complete(asyncio.gather(configure_and_run(vm_for_snapshot, {"config": network_config, "iface_id": "0"}, run=True, orig_ips=None)))

    ssh_conn = net_tools.SSHConnection(vm_for_snapshot.ssh_config)
    ssh_conn.scp_file(
        "../resources/tests/fib.py", "./fib.py"
    )

    vm_for_snapshot.pause_to_snapshot(
        mem_file_path=mem_fname,
        snapshot_path=vmstate_fname,
        diff=False,
    )

    shutil.rmtree(shared_dir_name, ignore_errors=True)
    os.makedirs(shared_dir_name)

    chroot_dir = vm_for_snapshot.chroot()
    shutil.copyfile(
        Path(chroot_dir) / mem_fname,
        Path(shared_dir_name) / mem_fname,
    )
    shutil.copyfile(
        Path(chroot_dir) / vmstate_fname,
        Path(shared_dir_name) / vmstate_fname,
    )

    t, hip, gip = result[0]
    orig_ips = {'host_ip': hip, 'guest_ip': gip}

    return orig_ips

async def restore_microvm(microvm):
    chroot_dir = microvm.chroot()
    tmp_snapshot_dir = (
        Path() / chroot_dir / "tmp"
    )
    os.makedirs(tmp_snapshot_dir)

    mem_fname_in_jail = Path(tmp_snapshot_dir) / mem_fname
    vmstate_fname_in_jail = (
        Path(tmp_snapshot_dir) / vmstate_fname
    )

    shutil.copyfile(
        Path(shared_dir_name) / mem_fname,
        mem_fname_in_jail,
    )
    shutil.copyfile(
        Path(shared_dir_name) / vmstate_fname,
        vmstate_fname_in_jail,
    )
    
    microvm.restore_from_snapshot(
        snapshot_mem=mem_fname_in_jail,
        snapshot_vmstate=vmstate_fname_in_jail,
        snapshot_disks=[microvm.rootfs_file],
        snapshot_is_diff=True,
    )

    return None, None

def collect_pid_stats(pid):
    _, stdout, _ = run_cmd(f"ps -o pid,min_flt,maj_flt | grep {pid} | awk '{{print $2, $3}}'")
    min_flt, maj_flt = stdout.split()
    return int(min_flt), int(maj_flt)

async def restore_microvm_uffd(microvm, uffd_handler_paths):
    chroot_dir = microvm.chroot()
    tmp_snapshot_dir = (
        Path() / chroot_dir / "tmp"
    )
    os.makedirs(tmp_snapshot_dir)

    mem_fname_in_jail = Path(tmp_snapshot_dir) / mem_fname
    vmstate_fname_in_jail = (
        Path(tmp_snapshot_dir) / vmstate_fname
    )

    shutil.copyfile(
        Path(shared_dir_name) / mem_fname,
        mem_fname_in_jail,
    )
    shutil.copyfile(
        Path(shared_dir_name) / vmstate_fname,
        vmstate_fname_in_jail,
    )

    jailed_vmstate = microvm.create_jailed_resource(vmstate_fname_in_jail)
    microvm.create_jailed_resource(microvm.rootfs_file)

    _pf_handler = spawn_pf_handler(
        microvm, uffd_handler_paths["valid_handler"], mem_fname_in_jail
    )

    # collect pid stats for uffd handler
    pf_hanlder_pid = _pf_handler.proc().pid
    # min_flt, maj_flt = collect_pid_stats(pf_hanlder_pid)

    response = microvm.snapshot.load(
        mem_backend={"type": SnapshotMemBackendType.UFFD, "path": SOCKET_PATH},
        snapshot_path=jailed_vmstate,
        diff=False,
        resume=True,
    )
    # print(response.text)

    """ ret, stdout, stderr = run_cmd(f"ps -A")
    print(f"after:\n{stdout}") """

    assert response.ok

    return pf_hanlder_pid, _pf_handler

def restore_microvms(microvms, loop, uffd, uffd_handler_paths=None):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        if uffd:
            cmds.append(restore_microvm_uffd(microvms[i], uffd_handler_paths=uffd_handler_paths))
        else:
            cmds.append(restore_microvm(microvms[i]))

    start = time.time()
    results = loop.run_until_complete(asyncio.gather(*cmds))
    end = time.time()
    print(f"time restore: {end - start}")

    return results
    """ for r in results:
        print(r.stdout) """

@pytest.mark.timeout(3 * 60 * 60)
@decorators.test_context("api", NO_OF_MICROVMS)
# @pytest.mark.skipif(True, reason="debug")
def test_run_concurrency_snap(test_multiple_microvms, microvm, network_config):
    """
    Check we can spawn multiple microvms.

    @type: functional
    """
    microvms = test_multiple_microvms
    uvm_data = []

    loop = set_up_event_loop()

    orig_ips = create_snapshot(microvm, network_config)
    configure_microvms(microvms, loop, network_config, run=False, orig_ips=orig_ips)
    restore_microvms(microvms, loop, uffd=False)
    connect_to_microvms(microvms, uvm_data, loop)
    # run_bin_on_microvms_dbg(uvm_data, loop)
    run_bin_on_microvms(uvm_data, loop, NO_OF_FIB_PREWARM, "prewarm")
    run_stats(uvm_data, loop, NO_OF_RUNS)

def run_stats(uvm_data, loop, num_runs):
    times = []

    for i in range(num_runs):
        res = run_bin_on_microvms(uvm_data, loop, NO_OF_FIB_RUN, "fib")
        times.append(res)

    a = np.array(times)
    print(f"Results:\n\
- mean: {a.mean()}\n\
- std: {a.std()}\n\
- P50: {np.percentile(a, 50)}\n\
- P90: {np.percentile(a, 90)}\n\
- P99: {np.percentile(a, 99)}"
    )

def run_pid_stats(stats):
    for pid, old_min_flt, old_maj_flt, pf_handler in stats:
        new_min_flt, new_maj_flt = collect_pid_stats(pid)
        print(f"pid {pid} old {old_min_flt} {old_maj_flt} new {new_min_flt} {new_maj_flt} diff {new_min_flt - old_min_flt} {new_maj_flt - old_maj_flt}")

def capture_pid_stats_before(pid_proc):
    stats = []
    for pid, proc in pid_proc:
        min_flt, maj_flt = collect_pid_stats(pid)
        stats.append((pid, min_flt, maj_flt, proc))
    return stats

@pytest.mark.timeout(3 * 60 * 60)
@decorators.test_context("api", NO_OF_MICROVMS)
# @pytest.mark.skipif(True, reason="debug")
def test_run_concurrency_snap_uffd(test_multiple_microvms, microvm, network_config, uffd_handler_paths):
    """
    Check we can spawn multiple microvms.

    @type: functional
    """
    microvms = test_multiple_microvms
    uvm_data = []

    loop = set_up_event_loop()

    orig_ips = create_snapshot(microvm, network_config)
    configure_microvms(microvms, loop, network_config, run=False, orig_ips=orig_ips)
    pid_proc = restore_microvms(microvms, loop, uffd=True, uffd_handler_paths=uffd_handler_paths)
    connect_to_microvms(microvms, uvm_data, loop)
    # run_bin_on_microvms_dbg(uvm_data, loop)
    run_bin_on_microvms(uvm_data, loop, NO_OF_FIB_PREWARM, "prewarm")
    stats = capture_pid_stats_before(pid_proc)
    run_stats(uvm_data, loop, NO_OF_RUNS)
    run_pid_stats(stats)

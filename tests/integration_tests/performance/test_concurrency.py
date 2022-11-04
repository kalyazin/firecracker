# Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Ensure multiple microVMs work correctly when spawned simultaneously."""

import asyncio
import asyncssh
from nsenter import Namespace
import socket
import time
import pytest

from framework import decorators

import host_tools.network as net_tools

NO_OF_MICROVMS = 48


def set_up_event_loop():
    try:
        loop = asyncio.get_event_loop()
    except RuntimeError:
        # Create event loop when one is not available
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

    return loop

async def configure_and_run(microvm, network_info):
    """Auxiliary function for configuring and running microVM."""
    microvm.spawn(create_logger=False)

    # Machine configuration specified in the SLA.
    config = {"vcpu_count": 1, "mem_size_mib": 128}

    microvm.basic_config(**config)

    _tap, _, _ = microvm.ssh_network_config(
        network_info["config"], network_info["iface_id"]
    )

    microvm.start()
    return _tap

async def connect(username, identity, sock):
    return await asyncssh.connect(
        username=username, known_hosts=None, connect_timeout=1, client_keys=[identity], sock=sock
    )

async def execute_command(conn, cmd):
    result = await conn.run(cmd)
    return result

async def push_file(conn, src, dst):
    await asyncssh.scp(src, (conn, dst))

def configure_microvms(microvms, loop, network_config):
    cmds = []
    for i in range(NO_OF_MICROVMS):
        microvm = microvms[i]
        cmds.append(configure_and_run(microvm, {"config": network_config, "iface_id": str(i)}))
        # print(f"netns_file_path: {microvm.ssh_config['netns_file_path']}")

    start = time.time()
    results = loop.run_until_complete(asyncio.gather(*cmds))
    end = time.time()
    print(f"time config: {end - start}")

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
        cmds.append(execute_command(uvm_data[i]["assh_conn"], f"python ./fib.py {arg}"))

    start = time.time()
    results = loop.run_until_complete(asyncio.gather(*cmds))
    """ for r in results:
        print(r.stdout) """
    end = time.time()
    print(f"time {log}: {end - start}")

# @pytest.mark.timeout(20)
@decorators.test_context("api", NO_OF_MICROVMS)
def test_run_concurrency(test_multiple_microvms, network_config):
    """
    Check we can spawn multiple microvms.

    @type: functional
    """
    loop = set_up_event_loop()

    microvms = test_multiple_microvms

    uvm_data = []

    configure_microvms(microvms, loop, network_config)
    connect_to_microvms(microvms, uvm_data, loop)
    # run_bin_on_microvms_dbg(uvm_data, loop)
    push_bin_to_microvms(uvm_data, loop)
    run_bin_on_microvms(uvm_data, loop, 10, "prewarm")
    run_bin_on_microvms(uvm_data, loop, 36, "fib")

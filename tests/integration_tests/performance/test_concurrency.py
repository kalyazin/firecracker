# Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Ensure multiple microVMs work correctly when spawned simultaneously."""

import asyncio
import asyncssh
from nsenter import Namespace
import sys
import socket

from framework import decorators

import host_tools.network as net_tools

NO_OF_MICROVMS = 20


def set_up_event_loop():
    try:
        loop = asyncio.get_event_loop()
    except RuntimeError:
        # Create event loop when one is not available
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

    return loop

async def upload_and_run(ssh_conn, bin, cmd):
    ssh_conn.scp_file("../resources/tests/fib.py", "./fib.py")
    _, _, stderr = ssh_conn.execute_command(cmd)
    return stderr

async def run_ssh_cmd(ssh_conn, cmd):
    _, _, stderr = ssh_conn.execute_command(cmd)
    return stderr

@decorators.test_context("api", NO_OF_MICROVMS)
def test_run_concurrency(test_multiple_microvms, network_config):
    """
    Check we can spawn multiple microvms.

    @type: functional
    """
    loop = set_up_event_loop()

    cmds = []

    microvms = test_multiple_microvms

    # import
    for i in range(NO_OF_MICROVMS):
        microvm = microvms[i]
        _ = _configure_and_run(microvm, {"config": network_config, "iface_id": str(i)})
        print(f"netns_file_path: {microvm.ssh_config['netns_file_path']}")
        # import_key(microvm.ssh_config['ssh_key_path'])

    for i in range(NO_OF_MICROVMS):
        microvm = microvms[i]
        # _ = _configure_and_run(microvm, {"config": network_config, "iface_id": str(i)})
        # We check that the vm is running by testing that the ssh does
        # not time out.
        ssh_conn = net_tools.SSHConnection(microvm.ssh_config)

        with Namespace(microvm.ssh_config['netns_file_path'], "net"):
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.connect((microvm.ssh_config['hostname'], 22));
        cmds.append(run_client(
            microvm.ssh_config['hostname'], microvm.ssh_config['username'],
            microvm.ssh_config['ssh_key_path'], microvm.ssh_config['netns_file_path'],
            sock
        ))

        """ cmds.append(run_ssh_cmd(ssh_conn, "/bin/ps")) """
        """ print(f"username: {microvm.ssh_config['username']}")
        print(f"hostname: {microvm.ssh_config['hostname']}")
        print(f"ssh_key_path: {microvm.ssh_config['ssh_key_path']}") """
        # cmds.append(upload_and_run(ssh_conn, "", "python ./fib.py 1 > /dev/null"))

    """ try:
        asyncio.get_event_loop().run_until_complete(run_client())
    except (OSError, asyncssh.Error) as exc:
        sys.exit('SSH connection failed: ' + str(exc)) """
    # ns = microvms[0].ssh_config['netns_file_path']
    # with Namespace(ns, "net"):
    results = loop.run_until_complete(asyncio.gather(*cmds))
    """ for result in results:
        assert result.read() == "" """


def _configure_and_run(microvm, network_info):
    """Auxiliary function for configuring and running microVM."""
    microvm.spawn()

    # Machine configuration specified in the SLA.
    config = {"vcpu_count": 1, "mem_size_mib": 128}

    microvm.basic_config(**config)

    _tap, _, _ = microvm.ssh_network_config(
        network_info["config"], network_info["iface_id"]
    )

    microvm.start()
    return _tap

def import_key(identity_fname):
    with open(identity_fname, "r") as f:
        asyncssh.import_private_key(f.read())

async def run_client(hostname, username, identity, ns, sock) -> None:
    # with Namespace(ns, "net"):
    """ async with asyncssh.connect(
        hostname, username=username, known_hosts=None, connect_timeout=1, client_keys=[identity],
    ) as conn: """
    async with asyncssh.connect(
        username=username, known_hosts=None, connect_timeout=1, client_keys=[identity], sock=sock
    ) as conn:
        result = await conn.run('ps', check=True)
        print(result.stdout, end='')

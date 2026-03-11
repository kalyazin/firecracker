# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Tests the network latency of a Firecracker guest."""

import json
import re
from pathlib import Path

import pytest

from framework.utils_iperf import IPerf3Test, emit_iperf3_metrics


def consume_ping_output(ping_putput):
    """Consume ping output.

    Output example:
    PING 8.8.8.8 (8.8.8.8) 56(84) bytes of data.
    64 bytes from 8.8.8.8: icmp_seq=1 ttl=118 time=17.7 ms
    64 bytes from 8.8.8.8: icmp_seq=2 ttl=118 time=17.7 ms
    64 bytes from 8.8.8.8: icmp_seq=3 ttl=118 time=17.4 ms
    64 bytes from 8.8.8.8: icmp_seq=4 ttl=118 time=17.8 ms

    --- 8.8.8.8 ping statistics ---
    4 packets transmitted, 4 received, 0% packet loss, time 3005ms
    rtt min/avg/max/mdev = 17.478/17.705/17.808/0.210 ms
    """
    output = ping_putput.strip().split("\n")
    assert len(output) > 2

    # Compute percentiles.
    pattern_time = ".+ bytes from .+: icmp_seq=.+ ttl=.+ time=(.+) ms"
    for seq in output:
        time = re.findall(pattern_time, seq)
        if time:
            assert len(time) == 1
            yield float(time[0])


@pytest.fixture
def network_microvm(request, uvm_plain_acpi):
    """Creates a microvm with the networking setup used by the performance tests in this file.
    This fixture receives its vcpu count via indirect parameterization"""

    guest_mem_mib = 1024
    guest_vcpus = request.param

    vm = uvm_plain_acpi
    vm.spawn(log_level="Info", emit_metrics=True)
    vm.basic_config(vcpu_count=guest_vcpus, mem_size_mib=guest_mem_mib)
    vm.add_net_iface()
    vm.start()
    vm.pin_threads(0)

    return vm


@pytest.mark.nonci
@pytest.mark.parametrize("network_microvm", [1], indirect=True)
def test_network_latency(network_microvm, metrics):
    """
    Test network latency by sending pings from the guest to the host.
    """

    rounds = 15
    request_per_round = 30
    delay = 0.0

    metrics.set_dimensions(
        {
            "performance_test": "test_network_latency",
            **network_microvm.dimensions,
        }
    )

    samples = []
    host_ip = network_microvm.iface["eth0"]["iface"].host_ip

    for _ in range(rounds):
        _, ping_output, _ = network_microvm.ssh.check_output(
            f"ping -c {request_per_round} -i {delay} {host_ip}"
        )

        samples.extend(consume_ping_output(ping_output))

    for sample in samples:
        metrics.put_metric("ping_latency", sample, "Milliseconds")

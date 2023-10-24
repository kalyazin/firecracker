# Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Tests scenario for adding the maximum number of devices to a microVM."""

import platform

import pytest

# IRQs are available from 5 to 23, so the maximum number of devices
# supported at the same time is 19.
MAX_DEVICES_ATTACHED = 65


""" @pytest.mark.skipif(
    platform.machine() != "x86_64", reason="Firecracker supports 24 IRQs on x86_64."
) """
def test_attach_maximum_devices(test_microvm_with_api):
    """
    Test attaching maximum number of devices to the microVM.
    """
    test_microvm = test_microvm_with_api
    test_microvm.spawn()

    # Set up a basic microVM.
    test_microvm.basic_config()

    # Add (`MAX_DEVICES_ATTACHED` - 1) devices because the rootfs
    # has already been configured in the `basic_config()`function.
    for _ in range(MAX_DEVICES_ATTACHED - 1):
        test_microvm.add_net_iface()
    test_microvm.start()

    # Test that network devices attached are operational.
    for i in range(MAX_DEVICES_ATTACHED - 1):
        # Verify if guest can run commands.
        exit_code, _, _ = test_microvm.ssh_iface(i).run("sync")
        assert exit_code == 0


@pytest.mark.skipif(
    platform.machine() != "x86_64", reason="Firecracker supports 24 IRQs on x86_64."
)
def test_attach_too_many_devices(test_microvm_with_api):
    """
    Test attaching to a microVM more devices than available IRQs.
    """
    test_microvm = test_microvm_with_api
    test_microvm.spawn()

    # Set up a basic microVM.
    test_microvm.basic_config()

    # Add `MAX_DEVICES_ATTACHED` network devices on top of the
    # already configured rootfs.
    for _ in range(MAX_DEVICES_ATTACHED):
        test_microvm.add_net_iface()

    # Attempting to start a microVM with more than
    # `MAX_DEVICES_ATTACHED` devices should fail.
    error_str = (
        "Failed to allocate requested resource: The requested resource"
        " is not available."
    )
    with pytest.raises(RuntimeError, match=error_str):
        test_microvm.start()

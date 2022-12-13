# Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0

"""Tests for the verifying features exposed by CPUID and MSRs by various CPU templates."""

import pytest

from conftest import _test_images_s3_bucket
from framework.artifacts import ArtifactCollection, DiskArtifact
from framework.builder import MicrovmBuilder
import framework.utils_cpuid as cpuid_utils
from framework.utils_cpu_templates import SUPPORTED_CPU_TEMPLATES
import host_tools.network as net_tools


# CPU templates designed to provide instruction set feature parity
INST_SET_TEMPLATES = []

ARTIFACTS = ArtifactCollection(_test_images_s3_bucket())


@pytest.fixture(
    name="microvm",
    params=ARTIFACTS.microvms(keyword="1vcpu_1024mb"),
    ids=lambda uvm: uvm.name(),
)
def microvm_fxt(request):
    """Common microvm fixture for tests in this file"""
    uvm = request.param
    uvm.download()
    return uvm


@pytest.fixture(
    name="disk",
    params=ARTIFACTS.disks(keyword="bionic-msrtools"),
    ids=lambda disk: disk.name() if isinstance(disk, DiskArtifact) else None,
)
def disk_fxt(request):
    """Common disk fixture for tests in this file"""
    disk = request.param
    disk.download()
    return disk


def create_vm(bin_cloner_path, cpu_template, microvm, kernel, disk):
    """
    Create a VM.
    """
    root_disk = disk.copy()

    vm_builder = MicrovmBuilder(bin_cloner_path)
    vm_instance = vm_builder.build(
        kernel=kernel,
        disks=[root_disk],
        ssh_key=disk.ssh_key(),
        config=microvm,
        cpu_template=cpu_template,
    )
    vm = vm_instance.vm

    return vm


def check_cpuid_feat_flags(
    bin_cloner_path, cpu_template, microvm, kernel, disk, must_be_set, must_be_unset
):
    """
    Check that CPUID feature flag are set and unset as expected.
    """
    vm = create_vm(bin_cloner_path, cpu_template, microvm, kernel, disk)
    vm.start()

    cpuid = cpuid_utils.get_guest_cpuid(vm)
    allowed_regs = ["eax", "ebx", "ecx", "edx"]

    for leaf, subleaf, reg, flags in must_be_set:
        assert reg in allowed_regs
        actual = cpuid[(leaf, subleaf, reg)] & flags
        expected = flags
        assert (
            actual == expected
        ), f"{leaf=:#x} {subleaf=:#x} {reg=} {actual=:#x}, {expected=:#x}"

    for leaf, subleaf, reg, flags in must_be_unset:
        assert reg in allowed_regs
        actual = cpuid[(leaf, subleaf, reg)] & flags
        expected = 0
        assert (
            actual == expected
        ), f"{leaf=:#x} {subleaf=:#x} {reg=} {actual=:#x}, {expected=:#x}"


def test_feat_parity_cpuid_mpx(
    bin_cloner_path, cpu_template, microvm, guest_kernel, disk
):
    """
    Verify that MPX (Memory Protection Extensions) is not enabled in any of the supported CPU templates.

    @type: functional
    """
    # fmt: off
    must_be_set = []
    must_be_unset = [
        (0x7, 0x0, "ebx",
            (1 << 14) # MPX
        ),
    ]
    # fmt: on

    check_cpuid_feat_flags(
        bin_cloner_path,
        cpu_template,
        microvm,
        guest_kernel,
        disk,
        must_be_set,
        must_be_unset,
    )

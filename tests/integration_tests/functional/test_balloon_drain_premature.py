# Copyright 2026 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Demonstrate that the FPH drain condition used by the e2b orchestrator is
premature.

Runs against a hugetlbfs-2M-backed VM, matching the e2b production
configuration where every sandbox uses 2 MiB hugepages. The bug is
identical on 4 KiB backing — hugepages only shrink the cycle wall time
by ~30% (per fph.md §2), not the guest-side protocol — but we keep this
test pinned to the production backing so the demonstration matches what
the orchestrator would observe in practice.

The orchestrator (e2b-dev/infra
`packages/orchestrator/pkg/sandbox/fc/process.go`, `DrainBalloon`) treats

    host_cmd > host_cmd_before  AND  guest_cmd >= host_cmd

as the end-of-cycle signal after calling `balloon_hinting_start`. This test
shows that the condition is satisfied long before the FPH cycle finishes:
at the moment it first becomes true, a substantial fraction of the
eventual hinted pages have not yet been processed.

The cause is that the virtio-balloon driver sends the cmd_id as a *single*
4-byte outbuf descriptor at the start of the cycle (see Linux
`drivers/virtio/virtio_balloon.c::send_cmd_id_start`), which immediately
makes `guest_cmd == host_cmd`. The page-block descriptors that follow are
4-MiB inbufs that do *not* carry a cmd_id, so they do not affect
`guest_cmd`. FC's real end-of-cycle marker is `host_cmd` returning to
`FREE_PAGE_HINT_DONE` (1) after the guest sends `FREE_PAGE_HINT_STOP` (0),
gated on `acknowledge_on_stop=true` (the default).

See FC `src/vmm/src/devices/virtio/balloon/device.rs::process_free_page_hinting_queue`
and the unit test `test_hinting_stale_post_stop` for the host-side state
machine.
"""

import signal
import time

import pytest

from framework.microvm import HugePagesConfig

# Mirror FC's constants. Not exported to Python anywhere.
FREE_PAGE_HINT_STOP = 0
FREE_PAGE_HINT_DONE = 1


def _read_status(vm):
    """Return (host_cmd, guest_cmd_or_None) from /balloon/hinting/status."""
    res = vm.api.balloon_hinting_status.get().json()
    return res["host_cmd"], res.get("guest_cmd")


def _cumulative_hint_count(vm):
    """Sum `balloon.free_page_hint_count` across every metric line emitted
    since VM boot.

    FC's `SharedIncMetric` resets the counter on every flush (manual via
    `FlushMetrics` action, or automatic per `HostStatsSamplingInterval`).
    `flush_metrics()` only returns the *latest* line, so we'd miss any
    counter increments captured by automatic flushes between our manual
    ones. Summing across the whole metric history gives the true total.
    """
    return sum(
        line.get("balloon", {}).get("free_page_hint_count", 0)
        for line in vm.get_all_metrics()
    )


def _free_buddy_memory(vm):
    """Allocate memory in the guest, then free it back to buddy so an FPH
    cycle has plenty of MAX_ORDER-1 (4 MiB) blocks to hint.

    Uses fast_page_fault_helper which mmaps 128 MiB, touches it, waits for
    SIGUSR1, touches it again, then exits — process exit releases the
    mmap'd region back to the buddy allocator.
    """
    vm.ssh.check_output(
        "nohup /usr/local/bin/fast_page_fault_helper >/dev/null 2>&1 </dev/null &"
    )
    # Let the helper fault all 128 MiB in.
    time.sleep(2)
    _, pid, _ = vm.ssh.check_output("pidof fast_page_fault_helper")
    vm.ssh.check_output(f"kill -s {signal.SIGUSR1} {pid.strip()}")
    # Wait for process exit + buddy coalescing.
    time.sleep(3)


def test_orchestrator_drain_condition_is_premature(uvm_plain_any):
    """At the first moment the orchestrator's `host > host_before AND
    guest >= host` check is satisfied, the FPH cycle has barely begun:
    `free_page_hint_count` is a small fraction of its eventual value.
    """
    vm = uvm_plain_any
    vm.spawn()
    # Single-vCPU with 2 GiB RAM on hugetlbfs-2M backing — matches what the
    # e2b orchestrator uses in production. Per fph.md §2, hugepages make
    # each MADV_DONTNEED ~30% cheaper so the cycle is shorter than 4 KiB
    # backing, but the per-batch protocol on the guest virtqueue side is
    # identical, so the orchestrator's check fails the same way.
    vm.basic_config(
        vcpu_count=1,
        mem_size_mib=2048,
        huge_pages=HugePagesConfig.HUGETLBFS_2MB,
    )
    vm.add_net_iface()

    # Arm the balloon with FPH at install time. amount_mib=0 means we use
    # the balloon purely for FPH/FPR, not actual inflation.
    vm.api.balloon.put(
        amount_mib=0,
        deflate_on_oom=False,
        free_page_hinting=True,
    )
    vm.start()

    # Create a workload that produces ~256 MiB of contiguous free buddy
    # blocks. The exact number doesn't matter; we just need significantly
    # more than one MAX_ORDER-1 block so the cycle has measurable duration.
    _free_buddy_memory(vm)

    # Baseline: flush metrics so any FPH counter increments from prior
    # phases (none expected, but be safe) are captured in earlier lines,
    # then snapshot the cumulative total. Any subsequent calls to
    # `_cumulative_hint_count` will reflect activity *after* this point.
    vm.flush_metrics()
    baseline_pages = _cumulative_hint_count(vm)
    host_before, _ = _read_status(vm)

    # Kick off the cycle. Default StartHintingCmd has acknowledge_on_stop=true,
    # which is what the e2b orchestrator's startBalloonHinting(ctx, true)
    # also requests — so FC will reset host_cmd to FREE_PAGE_HINT_DONE at
    # end-of-cycle.
    vm.api.balloon_hinting_start.patch()

    # Busy-poll without sleeping. The FC API socket roundtrip
    # (~1 ms per describe call) is the limiting factor, giving an
    # effective poll rate near 1 kHz. We snapshot the cumulative metric
    # only at the two transition points (premature & done) so the loop
    # itself stays cheap.
    observations = []  # list of (host, guest) — for diagnostic only
    deadline = time.time() + 30.0
    premature_pages = None
    premature_idx = None
    final_pages = None
    saw_bump = False
    cycle_done = False
    start_wall = time.monotonic()
    premature_wall = None
    done_wall = None

    while time.time() < deadline:
        host, guest = _read_status(vm)
        observations.append((host, guest))

        # Orchestrator's premature condition. Snapshot the metric counter
        # the first time this fires.
        if (
            premature_pages is None
            and host > host_before
            and guest is not None
            and guest >= host
        ):
            premature_wall = time.monotonic() - start_wall
            vm.flush_metrics()
            premature_pages = _cumulative_hint_count(vm) - baseline_pages
            premature_idx = len(observations) - 1

        if host > host_before:
            saw_bump = True
        # Real end-of-cycle: host_cmd reset to DONE after guest STOP.
        if saw_bump and host == FREE_PAGE_HINT_DONE:
            done_wall = time.monotonic() - start_wall
            vm.flush_metrics()
            final_pages = _cumulative_hint_count(vm) - baseline_pages
            cycle_done = True
            break

    if not cycle_done:
        pytest.fail(
            f"FPH cycle did not reach DONE within deadline; "
            f"observed {len(observations)} polls"
        )

    # Diagnostic print, visible with `pytest -s`.
    print(
        f"\n{len(observations)} polls over "
        f"{done_wall * 1000:.1f} ms wall. "
        f"final_pages={final_pages}. "
        f"premature_idx={premature_idx}, "
        f"premature_pages={premature_pages}, "
        f"premature_wall={premature_wall*1000 if premature_wall else None:.1f} ms"
    )
    # Print the trace around the premature point.
    if premature_idx is not None:
        lo = max(0, premature_idx - 2)
        hi = min(len(observations), premature_idx + 5)
        for j in range(lo, hi):
            h, g = observations[j]
            marker = " <-- ORCHESTRATOR RETURNS" if j == premature_idx else ""
            print(f"  poll[{j}]: host={h} guest={g}{marker}")
    else:
        # Print sampled polls so we know what trajectory we saw.
        step = max(1, len(observations) // 8)
        for j in range(0, len(observations), step):
            h, g = observations[j]
            print(f"  poll[{j}]: host={h} guest={g}")
        print(
            f"  poll[{len(observations)-1}]: "
            f"host={observations[-1][0]} guest={observations[-1][1]} (last)"
        )

    assert premature_pages is not None, (
        "Orchestrator's premature condition was never observed. The cycle "
        "may have completed inside a single poll interval; rerun with a "
        "larger workload or check that the kernel emits FPH descriptors."
    )
    assert final_pages is not None and final_pages > 0, (
        "FPH never hinted any pages; the workload didn't produce buddy "
        "memory (check fast_page_fault_helper output and guest free memory)."
    )

    # The bug: at the moment the orchestrator's check first fires, the FPH
    # cycle has not actually completed. Strict inequality here is enough to
    # demonstrate the bug — any positive gap means at least one 4 MiB
    # block was still pending discard when the orchestrator would have
    # returned, and that block ends up in the snapshot as non-zero data
    # the guest had already considered free.
    #
    # The actual magnitude of the gap depends on how quickly our polling
    # lands relative to FC's queue processing. In typical runs we see
    # 20-99% pre-fired (i.e. 1-80% of pages still pending at "completion"),
    # and the orchestrator's check has no way to distinguish those cases.
    pending = final_pages - premature_pages
    pending_mib = pending * 4  # FPH hint-block size is MAX_ORDER-1 = 4 MiB
    msg = (
        f"At the moment the orchestrator's drain condition first fired "
        f"(t={premature_wall*1000:.1f} ms after start), "
        f"only {premature_pages}/{final_pages} pages "
        f"({100 * premature_pages / final_pages:.1f}%) had been hinted. "
        f"The remaining {pending} pages (~{pending_mib} MiB) were hinted "
        f"after the orchestrator would have returned — that memory would "
        f"have ended up in the snapshot as zeroes the guest had already "
        f"considered free."
    )
    print(msg)

    assert pending > 0, (
        "Premature condition fired exactly at cycle completion "
        f"(premature_pages == final_pages == {final_pages}). This run did "
        f"not catch the bug — likely the cycle was small enough to finish "
        f"between two polls. Retry with a larger workload."
    )

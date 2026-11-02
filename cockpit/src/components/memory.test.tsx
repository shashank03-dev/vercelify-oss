import { render, screen } from "@testing-library/react";
import type { MemoryReport } from "../types";
import { MemoryCard } from "./Memory";

function report(): MemoryReport {
  return {
    node_id: "main", name: "test-server", policy_version: 1, profile: "2 GiB · runtime first", sample_age_secs: 3,
    os_reserve_mb: 384, safety_mb: 128, reason: null,
    sample: { epoch: "boot", measured_at: 100, total_mb: 2048, available_mb: 800, workload_limit_mb: 1664,
      disk_free_mb: 1000, host_ready: true, external_heavy: false, containers: [], host_grant_mb: null,
      host_available_mb: null, provider_overhead_mb: null, host_oom_kills: 0 },
    account: { available_mb: 300, workload_remaining_mb: 200, reserved_mb: 128, growth_mb: 200, committed_mb: 512, disk_remaining_mb: 488, heavy: false },
  };
}

it("keeps missing capacity unknown and does not call it free memory", () => {
  const r = report(); r.sample = null; r.account = null; r.reason = "Waiting for fresh measurements";
  render(<MemoryCard reports={[r]} />);
  expect(screen.getByTestId("capacity-main-available")).toHaveTextContent("Unknown");
  expect(screen.getByTestId("capacity-main-headroom")).toHaveTextContent("Unknown");
  expect(screen.getByText("Waiting for fresh measurements")).toBeInTheDocument();
});

it("distinguishes a missing app reading from a measured empty database category", () => {
  const r = report(); r.sample!.containers.push({ id: "one", owner: "dpl_one", category: "app", running: true,
    usage_mb: null, limit_mb: 256, oom_killed: null, finished_at: null, peak_mb: null, oom_events: null, cap_events: null });
  render(<MemoryCard reports={[r]} />);
  expect(screen.getByTestId("capacity-main-apps")).toHaveTextContent("Unknown");
  expect(screen.getByTestId("capacity-main-databases")).toHaveTextContent("0 MiB");
  expect(screen.getByTestId("capacity-main-headroom")).toHaveTextContent("200 MiB");
});

it("marks stale observations and refuses to show them as admission headroom", () => {
  const r = report(); r.sample_age_secs = 120; r.account = null; r.reason = "Capacity report is stale";
  render(<MemoryCard reports={[r]} />);
  expect(screen.getByText("Capacity report is stale")).toBeInTheDocument();
  expect(screen.getByTestId("capacity-main-headroom")).toHaveTextContent("Unknown");
  expect(screen.getByText(/120 seconds old/)).toBeInTheDocument();
});

it("shows explicit OOM evidence without claiming that its cause is proven", () => {
  const r = report(); r.sample!.containers.push({ id: "one", owner: "dpl_one", category: "app", running: false,
    usage_mb: null, limit_mb: 256, oom_killed: true, finished_at: "2026-11-02T12:00:00Z", peak_mb: null, oom_events: 1, cap_events: null });
  render(<MemoryCard reports={[r]} />);
  expect(screen.getByText(/Recorded OOM/)).toBeInTheDocument();
  expect(screen.queryByText(/cap exhaustion confirmed/i)).not.toBeInTheDocument();
});

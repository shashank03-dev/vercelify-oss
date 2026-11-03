import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RestoreDrills } from "./RestoreDrills";
import type { Database, DatabaseBackup } from "../types";
import type { PlatformClient } from "../lib/platform";
const gate=vi.hoisted(()=>({caps:["postgres-restore-drills"] as string[]}));
vi.mock("../lib/live",()=>({useLive:()=>({health:{data:{capabilities:gate.caps}}})}));
beforeEach(()=>{gate.caps=["postgres-restore-drills"];});
const db={id:"db_test",kind:"postgresql",version:"17"} as Database;
const backups=[{id:"bak_test",status:"succeeded",snapshot:"a".repeat(64),created_at:"2026-11-03T00:00:00Z"}] as DatabaseBackup[];
function client(jobs:unknown[]=[]){return {restoreDrills:vi.fn(async()=>jobs),startRestoreDrill:vi.fn(async()=>({})),cancelRestoreDrill:vi.fn(async()=>({})),restoreDrillSchedule:vi.fn(async()=>({schedule:{enabled:false,schedule:"0 3 1 * *",assertions:[],error:"",last_job:null},next_due:null,overdue:false}))} as unknown as PlatformClient;}
it("submits selected snapshot checks without using destructive production restore",async()=>{
  const c=client();render(<RestoreDrills db={db} backups={backups} client={c}/>);
  fireEvent.change(screen.getByLabelText("Read query"),{target:{value:"SELECT version FROM migrations"}});
  fireEvent.change(screen.getByLabelText("Expected result"),{target:{value:"20261103"}});
  fireEvent.click(screen.getByText("Run restore drill"));
  await waitFor(()=>expect(c.startRestoreDrill).toHaveBeenCalledWith("db_test","bak_test",[{name:"Known result",query:"SELECT version FROM migrations",expected:"20261103"}],false));
  expect(c.restoreDrillSchedule).not.toHaveBeenCalled();
});
it("keeps partial verification and cleanup visible without a success label",async()=>{
  const c=client([{id:"task_test",snapshot:"b".repeat(64),state:"cleanup",cleanup_pending:true,cancel_requested:false,verified_at:"2026-11-03T00:00:00Z",integrity:"metadata_checked",credential_source:"host",assertion_names:[],error:""}]);
  render(<RestoreDrills db={db} backups={backups} client={c}/>);
  expect(await screen.findByText(/cleanup pending/)).toBeVisible();
  expect(screen.getByText("No verified restore yet.")).toBeVisible();
  expect(screen.getByText("Run restore drill")).toBeDisabled();
  fireEvent.click(screen.getByText("Cancel drill"));
  await waitFor(()=>expect(c.cancelRestoreDrill).toHaveBeenCalledWith("task_test"));
});
it("makes no unsupported calls to old agents",()=>{
  gate.caps=[];const c=client();render(<RestoreDrills db={db} backups={backups} client={c}/>);
  expect(c.restoreDrills).not.toHaveBeenCalled();expect(screen.queryByText("Run restore drill")).not.toBeInTheDocument();
});
it("loads sealed schedule assertions only on an explicit owner configuration action",async()=>{
  const c=client();render(<RestoreDrills db={db} backups={backups} client={c}/>);
  fireEvent.click(screen.getByText("Configure schedule (owners)"));
  expect(await screen.findByLabelText("UTC schedule")).toHaveValue("0 3 1 * *");
  expect(screen.getByRole("checkbox", { name: /Schedule restore drills/ })).not.toBeChecked();
});

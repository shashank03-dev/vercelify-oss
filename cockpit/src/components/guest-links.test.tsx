import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { GuestLinksButton } from "./GuestLinks";
import type { PlatformClient } from "../lib/platform";
const gate=vi.hoisted(()=>({caps:["preview-guest-links"] as string[]}));
vi.mock("../lib/live",()=>({useLive:()=>({health:{data:{capabilities:gate.caps}}})}));
beforeEach(()=>{gate.caps=["preview-guest-links"];});
function client(domain:string|null="guests.example.test") {
  return {guestSettings:vi.fn(async()=>({config:{domain,public_port:8443},gateway_available:true})),guestInvitations:vi.fn(async()=>[]),configureGuestDomain:vi.fn(async()=>({})),createGuestInvitation:vi.fn(async()=>({grant:{id:"shr_test"},url:"https://unique.guests.example.test/__vfy/invite#vfy=shown-once"}))} as unknown as PlatformClient;
}
function open(c:PlatformClient) {render(<GuestLinksButton app="app_test" pr={7} url="http://pr-7.private.vfy" privatePreview client={c}/>);fireEvent.click(screen.getByText("Share"));}
it("issues an explicitly confirmed one-session invitation and forgets its secret when closed",async()=>{
  const c=client();open(c);
  await waitFor(()=>expect(screen.getByTestId("create-guest-invitation")).toBeEnabled());
  fireEvent.click(screen.getByTestId("create-guest-invitation"));
  expect(await screen.findByLabelText("Secret guest invitation")).toHaveValue("https://unique.guests.example.test/__vfy/invite#vfy=shown-once");
  expect(c.createGuestInvitation).toHaveBeenCalledWith("app_test",7,3600,true);
  fireEvent.click(screen.getByRole("button",{name:"Close"}));fireEvent.click(screen.getByText("Share"));
  expect(screen.queryByLabelText("Secret guest invitation")).not.toBeInTheDocument();
});
it("preserves an existing custom HTTPS port when editing a configured domain",async()=>{
  const c=client();open(c);
  await waitFor(()=>expect(screen.getByLabelText("Guest domain")).toHaveValue("guests.example.test"));
  expect(screen.getByLabelText("Public HTTPS port")).toHaveValue("8443");
  fireEvent.click(screen.getByText("Use guest domain"));
  await waitFor(()=>expect(c.configureGuestDomain).toHaveBeenCalledWith("guests.example.test",8443));
});
it("requires a configured guest domain while keeping the private link available",async()=>{
  open(client(null));await waitFor(()=>expect(screen.getByText(/No public guest domain/)).toBeVisible());
  expect(screen.getByTestId("create-guest-invitation")).toBeDisabled();
  expect(screen.getByText("http://pr-7.private.vfy")).toBeVisible();
});
it("does not call unsupported APIs on old agents",async()=>{
  gate.caps=[];const c=client();open(c);
  expect(c.guestSettings).not.toHaveBeenCalled();expect(c.guestInvitations).not.toHaveBeenCalled();
  await waitFor(()=>expect(screen.getByText(/Update this server/)).toBeVisible());
});

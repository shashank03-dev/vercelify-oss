import { DisplayCaster } from "../components/DisplayCaster";
import { PageHeader } from "../components/ui";

export function ScreencastView() {
  return (
    <>
      <PageHeader
        title="Second screen"
        subtitle="Use the Linux laptop's panel as a low-latency second monitor for your Mac, or pull the Linux desktop onto the Mac."
      />
      <DisplayCaster />
    </>
  );
}

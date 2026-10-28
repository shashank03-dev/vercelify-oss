import { DisplayCaster } from "../components/DisplayCaster";
import { PageHeader } from "../components/ui";

export function ScreencastView() {
  return (
    <>
      <PageHeader
        title="Second screen"
        subtitle="Share a Mac screen on Linux, or control the Linux desktop from your Mac."
      />
      <DisplayCaster />
    </>
  );
}

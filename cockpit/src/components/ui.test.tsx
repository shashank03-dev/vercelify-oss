import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { flushSync } from "react-dom";
import { Modal } from "./ui";

function Nested() {
  const [outer, setOuter] = useState(true);
  const [inner, setInner] = useState(true);
  return (
    <Modal open={outer} onClose={() => setOuter(false)} title="Outer">
      outer body
      <Modal open={inner} onClose={() => setInner(false)} title="Inner">
        inner body
      </Modal>
    </Modal>
  );
}

// As the app's build-log dialog: the inner modal is unmounted (not just
// closed) and the browser flushes React's update before the next listener.
function NestedUnmounted({ onOuterClose }: { onOuterClose: () => void }) {
  const [inner, setInner] = useState(true);
  return (
    <Modal open onClose={onOuterClose} title="Outer">
      outer body
      {inner && (
        <Modal open onClose={() => flushSync(() => setInner(false))} title="Inner">
          inner body
        </Modal>
      )}
    </Modal>
  );
}

describe("Modal", () => {
  it("Escape closes only the innermost modal", async () => {
    render(<Nested />);
    expect(screen.getByText("inner body")).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText("inner body")).not.toBeInTheDocument(), { timeout: 3000 });
    expect(screen.getByText(/outer body/)).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText(/outer body/)).not.toBeInTheDocument(), { timeout: 3000 });
  });

  it("Escape on an inner modal that unmounts leaves the outer one open", async () => {
    const onOuterClose = vi.fn();
    render(<NestedUnmounted onOuterClose={onOuterClose} />);
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText("inner body")).not.toBeInTheDocument());
    expect(onOuterClose).not.toHaveBeenCalled();
  });
});

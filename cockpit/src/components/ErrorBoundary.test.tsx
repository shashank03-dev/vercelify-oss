import { fireEvent, render, screen } from "@testing-library/react";
import { vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";

let broken = true;
function Flaky() {
  if (broken) throw new Error("agent sent something odd");
  return <p>page content</p>;
}

test("a page that throws shows the error and recovers with Try again", () => {
  const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  render(
    <div>
      <nav>sidebar</nav>
      <ErrorBoundary>
        <Flaky />
      </ErrorBoundary>
    </div>,
  );
  expect(screen.getByText("sidebar")).toBeInTheDocument();
  expect(screen.getByTestId("page-error")).toHaveTextContent("agent sent something odd");

  broken = false;
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(screen.getByText("page content")).toBeInTheDocument();
  quiet.mockRestore();
  warn.mockRestore();
});

test("the app-wide boundary offers a reload only", () => {
  broken = true;
  const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  render(
    <ErrorBoundary full>
      <Flaky />
    </ErrorBoundary>,
  );
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  expect(screen.getByRole("button", { name: "Reload the app" })).toBeInTheDocument();
  quiet.mockRestore();
  warn.mockRestore();
});

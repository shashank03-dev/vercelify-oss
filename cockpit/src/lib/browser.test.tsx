import { fireEvent, render, screen } from "@testing-library/react";
import { ToastProvider } from "../components/Toast";
import { useBrowser } from "./browser";

vi.mock("./open", () => ({ openExternal: () => Promise.reject(new Error("URL blocked by opener scope")) }));

it("shows a toast when the browser refuses an app link", async () => {
  function Link() {
    const browser = useBrowser();
    return <button onClick={() => void browser.open("http://shop.main.vfy/")}>Visit</button>;
  }
  render(<ToastProvider><Link /></ToastProvider>);
  fireEvent.click(screen.getByText("Visit"));
  expect(await screen.findByText(/Could not open browser:.*URL blocked/)).toBeInTheDocument();
});

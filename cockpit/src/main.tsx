import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { applyTheme, loadThemePref } from "./lib/theme";
import "./index.css";

// Before the first paint, so there is no flash of the wrong theme.
applyTheme(loadThemePref());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary full>
      <App />
    </ErrorBoundary>
  </StrictMode>,
);

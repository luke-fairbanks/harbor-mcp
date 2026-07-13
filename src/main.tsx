import React from "react";
import ReactDOM from "react-dom/client";
import { Theme } from "@radix-ui/themes";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "@radix-ui/themes/styles.css";
import "./styles.css";
import App from "./App";
import { TrayPanel } from "./components/TrayPanel";
import { useAppearance } from "./useAppearance";

class AppErrorBoundary extends React.Component<
  React.PropsWithChildren,
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error("Harbor UI failed to render", error);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="fatal-error" role="alert">
          <strong>Harbor hit an unexpected display error.</strong>
          <span className="mono">{this.state.error.message}</span>
          <button onClick={() => window.location.reload()}>
            Reload Harbor
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

// The same bundle renders the main app or the menu-bar panel, by window label.
const isTray = getCurrentWindow().label === "tray";

function Root() {
  const appearance = useAppearance();
  return (
    <Theme
      appearance={appearance}
      accentColor="blue"
      grayColor="slate"
      radius="medium"
      panelBackground="solid"
      scaling="100%"
    >
      {isTray ? <TrayPanel /> : <App />}
    </Theme>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <AppErrorBoundary>
      <Root />
    </AppErrorBoundary>
  </React.StrictMode>,
);

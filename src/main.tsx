import React from "react";
import ReactDOM from "react-dom/client";
import { Theme } from "@radix-ui/themes";
import "@radix-ui/themes/styles.css";
import "./styles.css";
import App from "./App";
import { useAppearance } from "./useAppearance";

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
      <App />
    </Theme>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);

import { Button, Spinner } from "@radix-ui/themes";
import {
  CheckCircledIcon,
  DownloadIcon,
  LockClosedIcon,
  UpdateIcon,
} from "@radix-ui/react-icons";
import type { AppUpdaterController } from "../useAppUpdater";
import { AnchorMark } from "./icons";

function statusCopy(updater: AppUpdaterController) {
  const { state } = updater;
  switch (state.phase) {
    case "checking":
      return "Checking GitHub for a signed Harbor release…";
    case "available":
      return `Version ${state.nextVersion} is ready to install.`;
    case "downloading":
      return "Downloading and verifying the update…";
    case "installing":
      return "Installing the verified update…";
    case "relaunching":
      return "Update installed. Relaunching Harbor…";
    case "restartRequired":
      return state.error || "Update installed. Restart Harbor to finish.";
    case "upToDate":
      return "You’re running the latest version.";
    case "error":
      return "The last update check did not complete.";
    default:
      return "Harbor checks for signed updates automatically.";
  }
}

export function AppSettingsPanel({
  updater,
}: {
  updater: AppUpdaterController;
}) {
  const { state } = updater;
  const busy =
    state.phase === "checking" ||
    state.phase === "downloading" ||
    state.phase === "installing" ||
    state.phase === "relaunching";
  const updateAvailable = state.phase === "available";
  const restartRequired = state.phase === "restartRequired";

  return (
    <div className="app-settings-page">
      <header className="page-header">
        <div className="page-header-copy">
          <p className="page-eyebrow">Harbor</p>
          <h1 className="page-title">Settings</h1>
          <p className="page-description">App preferences and secure updates.</p>
        </div>
      </header>

      <div className="detail-body app-settings-body">
        <section
          className="settings-update-card"
          aria-labelledby="updates-title"
        >
          <div className="settings-app-mark" aria-hidden>
            <AnchorMark size={22} />
          </div>
          <div className="settings-update-copy">
            <div className="settings-update-title-row">
              <h2 id="updates-title">Harbor updates</h2>
              <span>
                {state.currentVersion ? `v${state.currentVersion}` : "Version —"}
              </span>
            </div>
            <p>{statusCopy(updater)}</p>
            {state.phase === "error" && state.error && (
              <small>{state.error}</small>
            )}
          </div>
          <Button
            className="settings-update-action"
            size="2"
            variant={updateAvailable || restartRequired ? "solid" : "soft"}
            color={updateAvailable || restartRequired ? undefined : "gray"}
            disabled={busy}
            onClick={() =>
              void (restartRequired
                ? updater.restartApp()
                : updateAvailable
                  ? updater.installUpdate()
                  : updater.checkForUpdates(true))
            }
          >
            {busy ? (
              <Spinner size="1" />
            ) : restartRequired ? (
              <UpdateIcon />
            ) : updateAvailable ? (
              <DownloadIcon />
            ) : state.phase === "upToDate" ? (
              <CheckCircledIcon />
            ) : null}
            {state.phase === "checking"
              ? "Checking…"
              : state.phase === "downloading"
                ? "Downloading…"
                : state.phase === "installing"
                  ? "Installing…"
                  : state.phase === "relaunching"
                    ? "Relaunching…"
                    : updateAvailable
                      ? `Update to v${state.nextVersion}`
                      : restartRequired
                        ? "Restart Harbor"
                        : "Check for updates"}
          </Button>
        </section>

        <aside className="settings-signature-note">
          <LockClosedIcon aria-hidden />
          <p>
            Every update is validated with Harbor’s updater key, then protected by
            Faba Development’s Apple signature and notarization.
          </p>
        </aside>
      </div>
    </div>
  );
}

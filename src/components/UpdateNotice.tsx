import { Button, Spinner } from "@radix-ui/themes";
import {
  CheckCircledIcon,
  DownloadIcon,
  ExclamationTriangleIcon,
} from "@radix-ui/react-icons";
import type { AppUpdaterController } from "../useAppUpdater";

function releaseSummary(notes: string | null) {
  if (!notes) return "A new signed Harbor release is ready to install.";
  const line = notes
    .split("\n")
    .map((part) => part.trim())
    .find(
      (part) =>
        Boolean(part) &&
        !/^#{1,6}\s+(what'?s changed|new contributors)$/i.test(part) &&
        !/^\*\*(full changelog|install):/i.test(part) &&
        !/^universal macos build/i.test(part),
    );
  return (
    line
      ?.replace(/^#{1,6}\s+/, "")
      .replace(/^[-*]\s+/, "")
      .slice(0, 180) || "A new signed Harbor release is ready to install."
  );
}

export function UpdateNotice({
  updater,
}: {
  updater: AppUpdaterController;
}) {
  const { state } = updater;
  const visible =
    state.phase === "available" ||
    state.phase === "downloading" ||
    state.phase === "installing" ||
    state.phase === "relaunching" ||
    state.phase === "restartRequired" ||
    state.phase === "upToDate" ||
    state.phase === "error" ||
    (state.phase === "checking" && state.source === "manual");

  if (!visible) return null;

  const determinate =
    state.totalBytes !== null &&
    state.totalBytes > 0 &&
    state.downloadedBytes > 0;
  const progress = determinate
    ? Math.min(100, (state.downloadedBytes / state.totalBytes!) * 100)
    : null;
  const busy =
    state.phase === "downloading" ||
    state.phase === "installing" ||
    state.phase === "relaunching";

  let title = "Checking for updates";
  let description = "Looking for the latest signed Harbor release…";
  if (state.phase === "available") {
    title = `Harbor v${state.nextVersion} is ready`;
    description = releaseSummary(state.notes);
  } else if (state.phase === "downloading") {
    title = `Downloading Harbor v${state.nextVersion}`;
    description =
      progress === null
        ? "Preparing the secure update…"
        : `${Math.round(progress)}% downloaded`;
  } else if (state.phase === "installing") {
    title = "Installing update";
    description = "Harbor verified the download and is installing it now.";
  } else if (state.phase === "relaunching") {
    title = "Relaunching Harbor";
    description = "The update is installed. Harbor will reopen in a moment.";
  } else if (state.phase === "restartRequired") {
    title = "Update installed";
    description =
      state.error || "Restart Harbor to finish moving to the new version.";
  } else if (state.phase === "upToDate") {
    title = "Harbor is up to date";
    description = `You’re running the latest version${state.currentVersion ? `, v${state.currentVersion}` : ""}.`;
  } else if (state.phase === "error") {
    title = "Update couldn’t finish";
    description = state.error || "Harbor could not reach the update service.";
  }

  return (
    <aside
      className="update-notice"
      data-phase={state.phase}
      aria-label="Harbor update"
    >
      <div
        className="sr-only"
        role={state.phase === "error" ? "alert" : "status"}
        aria-live={state.phase === "error" ? "assertive" : "polite"}
        aria-atomic="true"
      >
        {state.phase === "error" ? `${title}. ${description}` : title}
      </div>
      <div className="update-notice-icon" aria-hidden>
        {state.phase === "error" ? (
          <ExclamationTriangleIcon />
        ) : state.phase === "upToDate" ||
          state.phase === "restartRequired" ? (
          <CheckCircledIcon />
        ) : state.phase === "available" ? (
          <DownloadIcon />
        ) : (
          <Spinner size="1" />
        )}
      </div>
      <div className="update-notice-content">
        <strong>{title}</strong>
        <p>{description}</p>
        {(state.phase === "available" || busy) && (
          <small>Running projects stay online while Harbor restarts.</small>
        )}
        {state.phase === "downloading" && (
          <div
            className="update-progress"
            role="progressbar"
            aria-label="Update download progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progress === null ? undefined : Math.round(progress)}
            data-indeterminate={progress === null ? true : undefined}
          >
            <span style={{ width: progress === null ? "32%" : `${progress}%` }} />
          </div>
        )}
        {(state.phase === "error" || state.phase === "restartRequired") &&
          state.errorDetails && (
            <details className="update-error-details">
              <summary>Technical details</summary>
              <code>{state.errorDetails}</code>
            </details>
          )}
        <div className="update-notice-actions">
          {state.phase === "available" && (
            <>
              <Button
                size="1"
                variant="soft"
                color="gray"
                onClick={updater.dismiss}
              >
                Later
              </Button>
              <Button size="1" onClick={() => void updater.installUpdate()}>
                Update and restart
              </Button>
            </>
          )}
          {(state.phase === "upToDate" || state.phase === "error") && (
            <Button
              size="1"
              variant="soft"
              color="gray"
              onClick={updater.dismiss}
            >
              Dismiss
            </Button>
          )}
          {state.phase === "error" && (
            <Button size="1" onClick={() => void updater.checkForUpdates(true)}>
              Retry
            </Button>
          )}
          {state.phase === "restartRequired" && (
            <Button size="1" onClick={() => void updater.restartApp()}>
              Restart Harbor
            </Button>
          )}
        </div>
      </div>
    </aside>
  );
}

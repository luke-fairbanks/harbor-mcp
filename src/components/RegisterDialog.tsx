import { useEffect, useState } from "react";
import { Button, Dialog, Spinner } from "@radix-ui/themes";
import { DownloadIcon, FileIcon } from "@radix-ui/react-icons";
import { api, pickFolder } from "../api";
import type { Detection } from "../types";
import { HarborBeacon, ProjectGlyph } from "./icons";

export function RegisterDialog({
  open,
  onOpenChange,
  onRegistered,
  initialDetection,
  initialError,
  scanning,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onRegistered: (name: string) => void;
  initialDetection?: Detection | null;
  initialError?: string | null;
  scanning?: boolean;
}) {
  const [path, setPath] = useState<string | null>(null);
  const [detection, setDetection] = useState<Detection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Seed from a dropped-folder detection (or its error) when opened that way.
  useEffect(() => {
    if (!open) return;
    if (initialDetection) {
      setDetection(initialDetection);
      setPath(initialDetection.proposed.root);
      setError(null);
    } else if (initialError) {
      setError(initialError);
    }
  }, [open, initialDetection, initialError]);

  function reset() {
    setPath(null);
    setDetection(null);
    setError(null);
  }

  async function choose() {
    const dir = await pickFolder("Choose a project folder");
    if (!dir) return;
    setPath(dir);
    setDetection(null);
    setError(null);
    setBusy(true);
    try {
      setDetection(await api.detectApp(dir));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function register() {
    if (!detection) return;
    setBusy(true);
    try {
      await api.registerApp(detection.proposed);
      onRegistered(detection.proposed.name);
      onOpenChange(false);
      reset();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function importJson() {
    if (!path) return;
    setBusy(true);
    setError(null);
    try {
      const cfg = await api.importApp(path);
      onRegistered(cfg.name);
      onOpenChange(false);
      reset();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(v) => {
        onOpenChange(v);
        if (!v) reset();
      }}
    >
      <Dialog.Content maxWidth="620px" className="register-dialog">
        <div className="register-heading">
          <Dialog.Title>Add a project</Dialog.Title>
          <Dialog.Description size="2" color="gray">
            Harbor detects services and commands in this folder. Review them
            before anything runs.
          </Dialog.Description>
        </div>

        {!detection ? (
          <button className="project-picker" onClick={choose} disabled={busy}>
            <HarborBeacon size={92} />
            <strong>
              {busy || scanning
                ? "Scanning project…"
                : "Choose a project folder"}
            </strong>
            <span>Or drop a folder anywhere on the Harbor window.</span>
            <span className="project-picker-action">
              {busy || scanning ? <Spinner size="1" /> : <FileIcon />}
              {busy || scanning ? "Inspecting files" : "Browse folders"}
            </span>
          </button>
        ) : (
          <div className="register-review">
            <div className="register-project-head">
              <ProjectGlyph name={detection.proposed.name} />
              <div>
                <strong>{detection.proposed.name}</strong>
                <span className="mono">{detection.proposed.root}</span>
              </div>
              <span className="chip" data-tone="accent">
                {detection.proposed.services.length} service
                {detection.proposed.services.length === 1 ? "" : "s"}
              </span>
            </div>

            <div
              className="register-services"
              role="list"
              aria-label="Detected services"
            >
              {detection.proposed.services.map((service) => (
                <div
                  className="register-service"
                  role="listitem"
                  key={service.name}
                >
                  <div className="register-service-name">
                    <span>{service.name}</span>
                    {service.port && <code>:{service.port}</code>}
                  </div>
                  <code className="register-service-command">
                    {service.command}
                  </code>
                </div>
              ))}
            </div>

            {detection.notes.length > 0 && (
              <div className="register-notes">
                {detection.notes.map((note, index) => (
                  <span key={index}>{note}</span>
                ))}
              </div>
            )}
          </div>
        )}

        {error && (
          <div className="async-notice mono" data-tone="danger" role="alert">
            {error}
          </div>
        )}

        <div className="register-footer">
          <div className="register-alternate">
            {path && (
              <Button
                variant="ghost"
                size="1"
                onClick={importJson}
                disabled={busy}
              >
                <DownloadIcon /> Import existing harbor.json
              </Button>
            )}
          </div>
          <div className="register-footer-actions">
            <Dialog.Close>
              <Button variant="soft" color="gray">
                Cancel
              </Button>
            </Dialog.Close>
            {detection && (
              <Button
                onClick={register}
                disabled={detection.proposed.services.length === 0 || busy}
              >
                Add to Harbor
              </Button>
            )}
          </div>
        </div>
      </Dialog.Content>
    </Dialog.Root>
  );
}

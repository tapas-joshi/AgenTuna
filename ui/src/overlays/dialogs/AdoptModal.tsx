import { useEffect, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import type { AdoptApplyView } from "../../generated/AdoptApplyView";
import type { AdoptPreviewView } from "../../generated/AdoptPreviewView";
import { commandErrorMessage } from "../../lib/commandError";
import { Modal } from "./Modal";

export interface AdoptModalProps {
  readonly path: string;
  readonly name?: string | null;
  readonly expectedHash?: string | null;
  readonly onAdoptSuccess: (result: AdoptApplyView) => void;
  readonly onRescan?: () => void;
  readonly onClose: () => void;
}

/**
 * Single-item adoption confirmation dialog (TC-025, overlay-adopt-item design).
 */
export function AdoptModal({
  path,
  name: itemPropName,
  expectedHash,
  onAdoptSuccess,
  onRescan,
  onClose,
}: AdoptModalProps): JSX.Element {
  const [preview, setPreview] = useState<AdoptPreviewView | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isStale, setIsStale] = useState(false);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    setIsStale(false);

    invoke<AdoptPreviewView>("adopt_preview", {
      path,
      name: itemPropName ?? null,
      expectedHash: expectedHash ?? null,
      handling: "link",
    })
      .then((view) => {
        if (active) {
          if (view) setPreview(view);
          setLoading(false);
        }
      })
      .catch((err: unknown) => {
        if (!active) return;
        const msg = commandErrorMessage(err, "The adoption review could not be loaded.");
        setError(msg);
        if (msg.toLowerCase().includes("stale") || msg.includes("StaleScanRecord")) {
          setIsStale(true);
        }
        setLoading(false);
      });

    return () => {
      active = false;
    };
  }, [path, itemPropName, expectedHash]);

  async function handleAdopt() {
    setBusy(true);
    setError(null);
    try {
      const outcome = await invoke<AdoptApplyView>("adopt_apply", {
        // Apply the previewed item: the digest the preview verified travels back as
        // expectedHash, so a file that changed after the review is refused, not adopted.
        path: preview?.source ?? path,
        name: itemPropName ?? null,
        expectedHash: (preview as { hash?: string | null } | null)?.hash ?? expectedHash ?? null,
        handling: "link",
      });
      onAdoptSuccess(outcome);
      onClose();
    } catch (err: unknown) {
      const msg = commandErrorMessage(err, "The native item could not be adopted.");
      setError(msg);
      if (msg.toLowerCase().includes("stale") || msg.includes("StaleScanRecord")) {
        setIsStale(true);
      }
    } finally {
      setBusy(false);
    }
  }

  const name = preview?.native_name ?? itemPropName ?? path.split("/").filter(Boolean).pop() ?? "item";
  const agentsText = preview?.agents.join(" + ") ?? "agent";
  return (
    <Modal
      open={true}
      onOpenChange={(isOpen) => {
        if (!isOpen) onClose();
      }}
      title={`Adopt ${name}`}
      description="Review the original and the destination first."
      footer={
        <div className="flex items-center justify-end gap-8">
          <Button variant="ghost" disabled={busy} onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={loading || busy || preview === null}
            onClick={() => void handleAdopt()}
          >
            {busy ? "Adopting…" : "Adopt"}
          </Button>
        </div>
      }
    >
      <div className="flex flex-col gap-16 py-8" data-slot="adopt-modal">
        {loading ? (
          <p role="status" className="m-0 text-13 text-text-secondary">
            Preparing adoption preview…
          </p>
        ) : error !== null ? (
          <div role="alert" className="flex flex-col gap-8 rounded-8 border border-danger/40 bg-danger/10 p-12">
            <p className="m-0 text-13 text-danger">{error}</p>
            {isStale && onRescan && (
              <div className="mt-4">
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => {
                    onRescan();
                    onClose();
                  }}
                >
                  Rescan agent folders
                </Button>
              </div>
            )}
          </div>
        ) : preview ? (
          <>
            <section className="flex flex-col gap-4">
              <span className="text-11 font-semibold uppercase tracking-wide text-text-muted">
                Original · Read-only
              </span>
              <p className="m-0 font-mono text-13 text-text">{preview.source}</p>
              <p className="m-0 text-12 text-text-secondary">
                {agentsText} {preview.agents.length > 1 ? "· one shared folder" : ""}
              </p>
            </section>

            <div aria-hidden="true" className="text-16 text-text-muted">
              ↓
            </div>

            <section className="flex flex-col gap-4">
              <span className="text-11 font-semibold uppercase tracking-wide text-text-muted">
                AgenTuna destination
              </span>
              <p className="m-0 font-mono text-13 text-text">{preview.store_destination}</p>
              <p className="m-0 text-12 text-text-secondary">
                master copy · committed with this repo
              </p>
            </section>

            <p className="m-0 text-12 text-text-secondary">
              <span className="font-semibold text-text">Backup: </span>
              <span className="font-mono text-text-muted">{preview.backup_dir}</span>
            </p>
            <hr className="my-4 border-border" />
            <section className="rounded-8 border border-border bg-raised p-12">
              <h3 className="m-0 text-13 font-semibold text-text">What changes</h3>
              <ul className="m-0 mt-6 flex list-disc flex-col gap-4 pl-16 text-12 text-text-secondary">
                <li>Original is copied, then replaced by a link</li>
                <li>{agentsText} {preview.agents.length > 1 ? "both" : "will"} keep loading it</li>
                <li>Undo restores it only if unchanged since adoption</li>
              </ul>
            </section>
          </>
        ) : null}
      </div>
    </Modal>
  );
}

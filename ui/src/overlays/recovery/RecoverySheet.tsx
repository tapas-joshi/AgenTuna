import type { JSX } from "react";
import type { RecoverySheetView } from "../../generated/RecoverySheetView";

/** User-facing review surface for interrupted operations discovered at startup. */
export function RecoverySheet({
  recovery,
  onContinue,
  onDiscard,
  onRestore,
}: {
  readonly recovery: RecoverySheetView;
  readonly onContinue: () => void;
  readonly onDiscard: (operationId: string) => void;
  readonly onRestore: (file: RecoverySheetView["files"][number]) => void;
}): JSX.Element {
  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/50 p-16" role="presentation">
      <section aria-labelledby="recovery-title" aria-modal="true" className="max-h-[85vh] w-full max-w-2xl overflow-auto rounded-8 border border-border bg-surface p-24 shadow-xl" role="dialog">
        <h2 className="m-0 text-18 font-semibold text-text" id="recovery-title">Recover interrupted changes</h2>
        <p className="mb-0 mt-8 text-14 text-text-muted">AgenTuna found work left unfinished by the previous session. Review the affected paths before continuing.</p>
        {recovery.operations.length === 0 && recovery.manual_review.length === 0 && recovery.temp_files.length === 0 ? (
          <p className="mt-16 text-14 text-text">No pending file changes need review.</p>
        ) : null}
        <ul className="mt-16 space-y-12">
          {recovery.operations.map((operation) => (
            <li className="rounded-6 border border-border p-12" key={operation.id}>
              <div className="flex items-center justify-between gap-12">
                <strong className="text-13 text-text">{operation.kind} · {operation.id}</strong>
                <button className="rounded-6 border border-border px-12 py-4 text-13 text-text" onClick={() => onDiscard(operation.id)} type="button">Discard pending</button>
              </div>
              <ul className="mt-8 list-inside list-disc text-13 text-text-secondary">
                {operation.paths.map((path) => <li key={path}>{path}</li>)}
              </ul>
            </li>
          ))}
        </ul>
        {recovery.files.length > 0 ? <section className="mt-16" aria-label="Files available for recovery"><h3 className="m-0 text-13 font-medium text-text">Affected files</h3><ul className="mt-8 space-y-8">{recovery.files.map((file) => <li className="flex items-center justify-between gap-12 text-13 text-text" key={`${file.operation_id}:${file.path}`}><span>{file.path} <span className="text-text-muted">({file.state})</span></span>{file.can_restore ? <button className="rounded-6 border border-border px-12 py-4 text-13 text-text" onClick={() => onRestore(file)} type="button">Restore</button> : null}</li>)}</ul></section> : null}
        {recovery.manual_review.length > 0 ? <section className="mt-16" aria-label="Manual review required"><h3 className="m-0 text-13 font-medium text-text">Manual review required</h3><ul className="mt-8 list-inside list-disc text-13 text-text-secondary">{recovery.manual_review.map((item) => <li key={item}>{item}</li>)}</ul></section> : null}
        {recovery.temp_files.length > 0 ? <section className="mt-16" aria-label="Temporary files"><h3 className="m-0 text-13 font-medium text-text">Temporary files</h3><ul className="mt-8 list-inside list-disc text-13 text-text-secondary">{recovery.temp_files.map((file) => <li key={file}>{file}</li>)}</ul></section> : null}
        <div className="mt-24 flex justify-end"><button className="rounded-6 bg-accent-fill px-16 py-8 text-13 font-medium text-on-accent" onClick={onContinue} type="button">Continue</button></div>
      </section>
    </div>
  );
}

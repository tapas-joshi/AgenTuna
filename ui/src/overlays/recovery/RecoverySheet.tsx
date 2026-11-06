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
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/50 p-4" role="presentation">
      <section aria-labelledby="recovery-title" aria-modal="true" className="max-h-[85vh] w-full max-w-2xl overflow-auto rounded-lg border bg-background p-6 shadow-xl" role="dialog">
        <h2 className="text-lg font-semibold" id="recovery-title">Recover interrupted changes</h2>
        <p className="mt-2 text-sm text-muted-foreground">AgenTuna found work left unfinished by the previous session. Review the affected paths before continuing.</p>
        {recovery.operations.length === 0 && recovery.manual_review.length === 0 && recovery.temp_files.length === 0 ? (
          <p className="mt-4 text-sm">No pending file changes need review.</p>
        ) : null}
        <ul className="mt-4 space-y-3">
          {recovery.operations.map((operation) => (
            <li className="rounded-md border p-3" key={operation.id}>
              <div className="flex items-center justify-between gap-3">
                <strong>{operation.kind} · {operation.id}</strong>
                <button className="rounded border px-3 py-1 text-sm" onClick={() => onDiscard(operation.id)} type="button">Discard pending</button>
              </div>
              <ul className="mt-2 list-inside list-disc text-sm">
                {operation.paths.map((path) => <li key={path}>{path}</li>)}
              </ul>
            </li>
          ))}
        </ul>
        {recovery.files.length > 0 ? <section className="mt-4" aria-label="Files available for recovery"><h3 className="font-medium">Affected files</h3><ul className="space-y-2">{recovery.files.map((file) => <li className="flex items-center justify-between gap-3 text-sm" key={`${file.operation_id}:${file.path}`}><span>{file.path} <span className="text-muted-foreground">({file.state})</span></span>{file.can_restore ? <button className="rounded border px-3 py-1" onClick={() => onRestore(file)} type="button">Restore</button> : null}</li>)}</ul></section> : null}
        {recovery.manual_review.length > 0 ? <section className="mt-4" aria-label="Manual review required"><h3 className="font-medium">Manual review required</h3><ul className="list-inside list-disc text-sm">{recovery.manual_review.map((item) => <li key={item}>{item}</li>)}</ul></section> : null}
        {recovery.temp_files.length > 0 ? <section className="mt-4" aria-label="Temporary files"><h3 className="font-medium">Temporary files</h3><ul className="list-inside list-disc text-sm">{recovery.temp_files.map((file) => <li key={file}>{file}</li>)}</ul></section> : null}
        <div className="mt-6 flex justify-end"><button className="rounded bg-primary px-4 py-2 text-primary-foreground" onClick={onContinue} type="button">Continue</button></div>
      </section>
    </div>
  );
}

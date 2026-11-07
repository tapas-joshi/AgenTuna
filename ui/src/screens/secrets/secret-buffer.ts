/**
 * In-memory secret text that a dialog releases when it closes.
 *
 * JavaScript strings are immutable, so the webview cannot overwrite the underlying bytes. This
 * wrapper makes the lifetime explicit and testable: closing the dialog calls
 * {@link SecretBuffer.zeroize}, which drops the reference immediately and records that the
 * close path ran.
 */
export class SecretBuffer {
  #value = "";
  #zeroized = false;

  /** Current text; `""` once {@link SecretBuffer.zeroize} ran. */
  get value(): string {
    return this.#value;
  }

  /** Whether the buffer was released after its last write. */
  get zeroized(): boolean {
    return this.#zeroized;
  }

  /** Replaces the held text with a new in-memory value. */
  set(value: string): void {
    this.#value = value;
    this.#zeroized = false;
  }

  /** Drops the held text so a closed dialog keeps no copy. */
  zeroize(): void {
    this.#value = "";
    this.#zeroized = true;
  }
}

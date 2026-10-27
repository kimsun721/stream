export class Emitter<Events extends Record<string, unknown>> {
  #listeners = new Map<keyof Events, Set<(value: never) => void>>();

  /** Returns a function that removes the listener. */
  on<K extends keyof Events>(event: K, listener: (value: Events[K]) => void): () => void {
    let set = this.#listeners.get(event);
    if (!set) this.#listeners.set(event, (set = new Set()));
    set.add(listener as (value: never) => void);
    return () => this.off(event, listener);
  }

  off<K extends keyof Events>(event: K, listener: (value: Events[K]) => void): void {
    this.#listeners.get(event)?.delete(listener as (value: never) => void);
  }

  protected emit<K extends keyof Events>(event: K, value: Events[K]): void {
    for (const listener of this.#listeners.get(event) ?? []) {
      // A listener that throws must not stop the others or the SDK's own work.
      try {
        (listener as (value: Events[K]) => void)(value);
      } catch (error) {
        console.error(error);
      }
    }
  }
}

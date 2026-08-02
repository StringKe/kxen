export type Unsub = () => void;

export interface RpcResponse {
  id?: string | number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

export interface StreamChunk {
  stream?: { id: string; seq: number };
  result?: unknown;
}

export class RpcError extends Error {
  constructor(
    message: string,
    readonly code: number,
    readonly data?: unknown,
  ) {
    super(message);
    this.name = "RpcError";
  }
}

export class TopicStream<T = unknown> {
  constructor(private readonly source: (handler: (payload: unknown) => void) => Promise<Unsub>) {}

  on(cb: (value: T) => void): Unsub {
    let cancelled = false;
    const ready = this.source((payload) => {
      if (!cancelled) cb(payload as T);
    });
    const safe = ready.catch(() => () => {});
    return () => {
      cancelled = true;
      void safe.then((unsub) => unsub());
    };
  }

  filter(predicate: (value: T) => boolean): TopicStream<T> {
    return new TopicStream<T>((handler) =>
      this.source((payload) => {
        if (predicate(payload as T)) handler(payload);
      }),
    );
  }

  map<U>(project: (value: T) => U): TopicStream<U> {
    return new TopicStream<U>((handler) =>
      this.source((payload) => handler(project(payload as T))),
    );
  }
}

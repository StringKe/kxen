import { invoke } from "@tauri-apps/api/core";

export interface WsEndpoint {
  port: number;
  token: string;
}

let endpointPromise: Promise<WsEndpoint> | null = null;

export function getEndpoint(): Promise<WsEndpoint> {
  if (endpointPromise) return endpointPromise;
  const request = invoke<WsEndpoint>("ws_port").then((endpoint) => {
    if (!Number.isInteger(endpoint.port) || endpoint.port <= 0 || endpoint.port > 65_535)
      throw new Error("websocket server is not ready");
    if (typeof endpoint.token !== "string" || endpoint.token.length === 0)
      throw new Error("websocket endpoint token is unavailable");
    return endpoint;
  });
  endpointPromise = request;
  void request.catch(() => resetEndpoint(request));
  return request;
}

export function resetEndpoint(expected?: Promise<WsEndpoint>): void {
  if (!expected || endpointPromise === expected) endpointPromise = null;
}

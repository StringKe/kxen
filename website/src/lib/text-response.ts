export function utf8Text(body: string, mediaType: "text/markdown" | "text/plain"): Response {
  return new Response(body, {
    headers: { "Content-Type": `${mediaType}; charset=utf-8` },
  });
}

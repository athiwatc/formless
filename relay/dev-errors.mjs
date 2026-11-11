// Dev-shell fault boundaries, not Worker/room admission policy. Each wrapper
// awaits the handler so Node's event emitter never receives a rejected promise.
function report(scope, error) {
  const kind = error instanceof Error ? error.name : "unknown error";
  // Do not include request bodies, URL inputs, credentials or claim headers.
  console.error(`relay: ${scope} request failed (${kind})`);
}

export async function httpBoundary(req, res, handler, log = report) {
  try {
    await handler(req, res);
  } catch (error) {
    log("HTTP", error);
    if (res.destroyed || res.writableEnded) return;
    if (res.headersSent) {
      res.destroy();
      return;
    }
    try {
      res.writeHead(500, {
        "content-type": "application/json; charset=utf-8",
        "connection": "close",
      });
      res.end(JSON.stringify({ error: { message: "Internal server error", type: "server_error", code: "internal_error" } }));
    } catch (_) {
      res.destroy();
    }
  }
}

export async function upgradeBoundary(req, socket, handler, log = report) {
  try {
    await handler(req, socket);
  } catch (error) {
    log("Upgrade", error);
    socket.destroy();
  }
}

const MCP_CONFIG_ENV = "FORMLESS_PI_MCP_CONFIG";
const BRIDGE_CUSTOM_TYPE = "formless-pi-bridge";
const BRIDGE_MESSAGE = "formless-pi-mcp-registered";

export default function registerFormlessPiBridge(pi) {
  if (typeof pi.registerMcpServer !== "function") {
    throw new Error("Formless Pi adapter requires Pi 0.99.0 or newer with pi.registerMcpServer()");
  }

  let config;
  try {
    config = JSON.parse(process.env[MCP_CONFIG_ENV] ?? "");
  } catch {
    throw new Error(`Formless Pi adapter could not parse ${MCP_CONFIG_ENV}`);
  }
  if (
    !config ||
    typeof config.name !== "string" ||
    typeof config.command !== "string" ||
    typeof config.cwd !== "string" ||
    !Array.isArray(config.args) ||
    !config.args.every((arg) => typeof arg === "string") ||
    !Array.isArray(config.envVars) ||
    !config.envVars.every((name) => typeof name === "string") ||
    (config.restoreLeadingAt !== undefined && typeof config.restoreLeadingAt !== "boolean")
  ) {
    throw new Error(`Formless Pi adapter received invalid ${MCP_CONFIG_ENV}`);
  }

  let restoreLeadingAt = config.restoreLeadingAt === true;
  pi.on("input", (event) => {
    if (!restoreLeadingAt) return;
    restoreLeadingAt = false;
    if (event.text.startsWith("\n@")) {
      return { action: "transform", text: event.text.slice(1) };
    }
  });

  pi.on("session_start", () => {
    const env = {};
    for (const name of config.envVars) {
      if (!Object.prototype.hasOwnProperty.call(process.env, name)) {
        throw new Error(`Formless MCP environment variable is unavailable: ${name}`);
      }
      env[name] = process.env[name];
    }

    pi.registerMcpServer(config.name, {
      type: "stdio",
      command: config.command,
      args: config.args,
      cwd: config.cwd,
      env,
      exposure: "direct",
    });
    pi.sendMessage(
      {
        customType: BRIDGE_CUSTOM_TYPE,
        content: BRIDGE_MESSAGE,
        display: false,
        details: { serverName: config.name },
      },
      { triggerTurn: false, deliverAs: "nextTurn" },
    );
  });
}

export const config = {
  runner: "local",
  specs: ["./spec/*.mjs"],
  maxInstances: 1,
  services: [["tauri", { appBinaryPath: process.env.APP_BIN, driverProvider: "embedded", embeddedPort: 14447 }]],
  capabilities: [{ browserName: "tauri", "tauri:options": { application: process.env.APP_BIN } }],
  framework: "mocha",
  reporters: [],
  logLevel: "warn",
  mochaOpts: { timeout: 60000 },
};

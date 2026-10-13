// Genuine OIDC provider implementation for isolated qualification only.
// Accounts and client credentials here are fixtures, never deployment settings.
import { generateKeyPairSync, randomBytes } from "node:crypto";
import Provider from "oidc-provider";

const port = Number(process.env.MOTION_TEST_OIDC_PORT || 8951);
if (!Number.isSafeInteger(port) || port < 1024 || port > 65535) {
  throw new Error("Invalid fixture port");
}
const issuer = `http://127.0.0.1:${port}`;
const redirect = process.env.MOTION_TEST_OIDC_REDIRECT_URI;
if (!redirect) throw new Error("MOTION_TEST_OIDC_REDIRECT_URI is required");
const callback = new URL(redirect);
if (
  callback.protocol !== "http:" ||
  callback.hostname !== "127.0.0.1" ||
  callback.username || callback.password || callback.search || callback.hash
) {
  throw new Error("Fixture callback must use literal HTTP loopback");
}

const { privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const key = privateKey.export({ format: "jwk" });
key.kid = randomBytes(16).toString("hex");
key.alg = "RS256";
key.use = "sig";
const accounts = new Set(["admin", "alice", "viewer", "denied", "second-admin"]);
const provider = new Provider(issuer, {
  clients: [{
    client_id: process.env.MOTION_TEST_OIDC_CLIENT_ID || "motion-local-fixture",
    client_secret: process.env.MOTION_TEST_OIDC_CLIENT_SECRET || "motion-local-fixture-secret",
    redirect_uris: [redirect],
    response_types: ["code"],
    grant_types: ["authorization_code"],
    token_endpoint_auth_method: "client_secret_basic",
    id_token_signed_response_alg: "RS256",
  }],
  jwks: { keys: [key] },
  cookies: { keys: [randomBytes(32).toString("hex")] },
  claims: {
    openid: ["sub", "auth_time"],
    email: ["email", "email_verified"],
    profile: ["name"],
  },
  pkce: { methods: ["S256"], required: () => true },
  features: { devInteractions: { enabled: true } },
  async findAccount(_ctx, accountId) {
    if (!accounts.has(accountId)) return undefined;
    return {
      accountId,
      async claims() {
        return {
          sub: accountId,
          email: `${accountId}@example.test`,
          email_verified: true,
          name: `Fixture ${accountId}`,
        };
      },
    };
  },
});

// The package's development interaction page is intentionally a local fixture;
// it accepts one of the account names above without a production password.
const server = provider.listen(port, "127.0.0.1", () => {
  process.stdout.write(`Motion OIDC fixture listening at ${issuer}\n`);
});
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => server.close(() => process.exit(0)));
}

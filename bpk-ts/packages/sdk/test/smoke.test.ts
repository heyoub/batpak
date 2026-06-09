import { describe, expect, it } from "vitest";

import {
  BANK_COMMIT,
  BankCommitRequest,
  call,
  decodeBytes,
  encodeHex,
  NETBAT_VERSION,
  Schema,
} from "../src/index.js";

describe("@batpak/sdk barrel", () => {
  it("re-exports client, schema, generated, and canonical surfaces", () => {
    expect(NETBAT_VERSION).toBe("NETBAT/1");
    expect(BANK_COMMIT.name).toBe("bank.commit");
    expect(typeof call).toBe("function");
    expect(typeof decodeBytes).toBe("function");
    expect(typeof encodeHex).toBe("function");
    expect(typeof BankCommitRequest).toBe("object");
    expect(Schema).toBeDefined();
  });
});

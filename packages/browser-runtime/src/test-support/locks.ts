import { expect } from "vitest";
import type { OwnerLockManager } from "../owner.js";

export class FakeLocks implements OwnerLockManager {
  private held = false;

  public constructor(private readonly expectedName: string) {}

  public async request<T>(name: string, options: { mode: "exclusive"; ifAvailable: true }, operation: (lock: { name: string } | null) => Promise<T>): Promise<T> {
    expect.soft(name).toBe(this.expectedName);
    expect.soft(options).toEqual({ mode: "exclusive", ifAvailable: true });
    if (this.held) return operation(null);
    this.held = true;
    try { return await operation({ name }); } finally { this.held = false; }
  }
}

import { vi } from "vitest";
import type { EnrollmentCore } from "../enrollment.js";

export function coreFake(invoke: EnrollmentCore["invoke"]) {
  return { invoke: vi.fn(invoke) } satisfies EnrollmentCore;
}

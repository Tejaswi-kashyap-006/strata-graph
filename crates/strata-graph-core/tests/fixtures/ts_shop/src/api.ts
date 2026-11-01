import { Invoice, roundMoney } from "@/billing";
import { TaxCalculator } from "./billing/tax.js";
import type { Repo } from "./core/base";
import lodash from "lodash";

export function checkout(items: number[]): number {
  const inv = new Invoice(items);
  inv.save();
  return inv.total();
}

export const quote = (calc: TaxCalculator, amount: number): number => calc.compute(amount);

export function makeInvoice(): Invoice {
  return new Invoice([]);
}

export function fresh(): number {
  return makeInvoice().total();
}

export async function load(repo: Repo<Invoice>): Promise<number> {
  repo.find("1");
  lodash.chunk([1], 1);
  lodash.chain([1]).render();
  return roundMoney(1);
}

import * as billing from "./billing/tax";

export function aliasUse(): string {
  // `void` is a local export alias in tax.ts, not Mocker's getter.
  return billing.void();
}

export function missingMember(): void {
  // tax.ts has no `render`; never a match to the views' render() methods.
  billing.render();
}

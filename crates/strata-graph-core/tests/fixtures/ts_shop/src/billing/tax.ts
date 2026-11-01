export const RATE = 0.18;

export class TaxCalculator {
  compute(amount: number): number {
    return roundMoney(amount * RATE);
  }
}

export function roundMoney(x: number): number {
  return Math.round(x * 100) / 100;
}

function _void(): string {
  return "void";
}

export { _void as void };

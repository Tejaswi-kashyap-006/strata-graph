import { Model } from "../core/base";
import * as tax from "./tax";

export default class Invoice extends Model {
  private calc = new tax.TaxCalculator();

  constructor(private items: number[]) {
    super();
  }

  subtotal(): number {
    return this.items.reduce((a, b) => a + b, 0);
  }

  total(): number {
    return this.subtotal() + this.calc.compute(this.subtotal());
  }

  save(): void {
    super.save();
    this.validate();
  }
}

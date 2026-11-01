export class HtmlView {
  render(): string {
    return "<p/>";
  }
}

export class JsonView {
  render(): string {
    return "{}";
  }
}

export class Registry {
  set(key: string): string {
    return key;
  }
}

export function show(view: any): string {
  return view.render();
}

export function touch(m: Map<string, number>): void {
  // A builtin method name on a library type, with a repo method of the same name.
  m.set("a", 1);
}

export function shadow(roundMoney: (x: number) => number): number {
  // The parameter shadows src/billing/tax.roundMoney.
  return roundMoney(1);
}

export class Mocker {
  get void(): string {
    return "mock";
  }
}

export class Scanner {
  scanOnce(): number {
    return 1;
  }
}

export function scanUntyped(s: any): number {
  // One method in the repo has this name, but the receiver is untyped.
  return s.scanOnce();
}

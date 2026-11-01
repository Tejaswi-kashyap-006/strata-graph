export abstract class Model {
  save(): void {
    this.write();
  }

  validate(): boolean {
    return true;
  }

  protected write(): void {}
}

export interface Repo<T> {
  find(id: string): T | undefined;
}

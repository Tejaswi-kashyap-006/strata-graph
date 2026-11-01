import { Repo } from "./base";

// An interface implementation, and a call made through the interface type.
export class MemoryRepo implements Repo<string> {
  private items: Record<string, string> = {};

  find(id: string): string | undefined {
    return this.items[id];
  }
}

export function lookup(repo: Repo<string>, id: string): string | undefined {
  return repo.find(id);
}

// Declarations: interfaces, declaration merging, enums, namespaces, a module
// block, and the undecided type alias and generic type parameter.

// gold: (j) an interface with property and method signatures.
export interface Survey {
  id: number;
  title: string;
  launch(): void;
}

// gold: (j) declaration merging: two interfaces share one qualified name.
export interface Settings {
  theme: string;
}

export interface Settings {
  locale: string;
}

// gold: (j) a regular enum and a const enum.
export enum Status {
  Draft,
  Active = "active",
}

export const enum Priority {
  Low = 1,
  High = 2,
}

// gold: (j) a namespace holding a const, a function and a nested namespace.
export namespace Geometry {
  export const ORIGIN = 0;

  export function area(width: number, height: number): number {
    return width * height;
  }

  export namespace Inner {
    export function scale(value: number): number {
      return value * 2;
    }
  }
}

// gold: (j) a module block, the older spelling of a namespace.
module Legacy {
  export function old(): number {
    return Geometry.ORIGIN;
  }
}

// gold: (u) a type alias: the MVP column does not name type aliases.
export type SurveyId = number | string;

// gold: (u) generics: a generic interface, class and function.
export interface Box<T> {
  value: T;
}

export class Registry<T extends Survey> {
  private items: T[] = [];

  add(item: T): void {
    this.items.push(item);
  }
}

export function first<T>(items: T[]): T | undefined {
  return items[0];
}

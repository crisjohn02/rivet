// gold: (t) ambient declarations in a declaration file.
declare function greet(name: string): string;

declare const VERSION: string;

declare class Widget {
  render(): void;
}

declare namespace Lib {
  function version(): string;

  interface Options {
    verbose: boolean;
  }
}

interface Theme {
  name: string;
}

// gold: (u) a string-named ambient module: its naming is undecided.
declare module "legacy-lib" {
  export function start(): void;
}

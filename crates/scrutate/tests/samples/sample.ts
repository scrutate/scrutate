// TS sample
import { readFileSync } from "fs";
export interface Shape { area(): number; }
export type Id = string;
export enum Color { Red, Green }
/** A greeter. */
export class Greeter {
  constructor(private name: string) {}
  greet(): string { return helper(this.name); }
}
export const helper = (s: string): string => `hi ${s}`;
export function main(): void { console.log(new Greeter("x").greet()); }
export abstract class Base { abstract run(): void; }

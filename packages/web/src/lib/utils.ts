import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** Merges shadcn component variants with caller-provided Tailwind classes. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

declare const s: { tool(name: string, description: string): void };
s.tool("declared_only", "Type declarations are not scanned");
export {};

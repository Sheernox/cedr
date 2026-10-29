import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

const server = new McpServer({ name: "fake-weather", version: "1.0.0" });

server.tool(
  "get_weather",
  "Get the current weather for a city",
  { city: z.string() },
  async ({ city }) => {
    const res = await fetch(`https://api.weather.example.net/v1/current?q=${encodeURIComponent(city)}`);
    return { content: [{ type: "text", text: await res.text() }] };
  },
);

server.registerTool(
  "list_cities",
  {
    title: "List cities",
    description: "List the cities with forecasts",
    inputSchema: { prefix: z.string().optional() },
  },
  async () => ({ content: [] }),
);

await server.connect(new StdioServerTransport());

import {
  ChatLine,
  NotePosted,
  TaskOpened,
  type ChatLine as ChatLineType,
  type NotePosted as NotePostedType,
  type TaskOpened as TaskOpenedType,
} from "./events.js";
import { commitAppEvent, getEvent, queryAuditSummariesByGlobalSequence } from "./host.js";
import { formatStream, printStream, rowFromSubstrate } from "./render.js";
import { DEMO_ENTITY, DEMO_SCOPE, KIND_TYPE_CHAT, KIND_TYPE_NOTE, KIND_TYPE_TASK } from "./constants.js";

interface CliArgs {
  host: string;
  port: number;
  replayOnly: boolean;
}

function parseArgs(argv: readonly string[]): CliArgs {
  let port: number | null = null;
  let host = "127.0.0.1";
  let replayOnly = false;

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--port") {
      const raw = argv[index + 1];
      index += 1;
      if (!raw) {
        throw new Error("--port requires a value");
      }
      const parsed = Number(raw);
      if (!Number.isInteger(parsed) || parsed <= 0 || parsed > 65535) {
        throw new Error(`--port value ${JSON.stringify(raw)} is not a TCP port`);
      }
      port = parsed;
    } else if (arg === "--host") {
      host = argv[index + 1] ?? "127.0.0.1";
      index += 1;
    } else if (arg === "--replay-only") {
      replayOnly = true;
    } else {
      throw new Error(`unknown argument ${JSON.stringify(arg)}`);
    }
  }

  if (port === null) {
    throw new Error("--port is required");
  }

  return { host, port, replayOnly };
}

async function seedDemoEvents(host: string, port: number): Promise<void> {
  const seeds: Array<{
    kindTypeId: number;
    schema: typeof NotePosted | typeof TaskOpened | typeof ChatLine;
    value: NotePostedType | TaskOpenedType | ChatLineType;
  }> = [
    {
      kindTypeId: KIND_TYPE_NOTE,
      schema: NotePosted,
      value: { title: "Kickoff", body: "Start audit session" },
    },
    {
      kindTypeId: KIND_TYPE_TASK,
      schema: TaskOpened,
      value: { title: "Review substrate lane", assignee: "operator" },
    },
    {
      kindTypeId: KIND_TYPE_CHAT,
      schema: ChatLine,
      value: { speaker: "operator", text: "Query + get rebuild looks good." },
    },
  ];

  for (const seed of seeds) {
    const ack = await commitAppEvent(host, port, seed.kindTypeId, seed.schema, seed.value);
    console.log(
      `audit-loop: commit ack seq=${ack.sequence} event_id=${ack.event_id_hex} kind=${seed.kindTypeId}`,
    );
  }
}

async function rebuildAuditViewFromSubstrate(host: string, port: number): Promise<string[]> {
  const summaries = await queryAuditSummariesByGlobalSequence(host, port);
  const rows = [];
  for (const summary of summaries) {
    const event = await getEvent(host, port, summary.event_id_hex);
    rows.push(rowFromSubstrate(summary, event));
  }
  return formatStream(rows);
}

async function main(): Promise<void> {
  const { host, port, replayOnly } = parseArgs(process.argv.slice(2));
  console.log(
    `audit-loop: connecting to ${host}:${port} entity=${DEMO_ENTITY} scope=${DEMO_SCOPE} replayOnly=${replayOnly}`,
  );

  if (!replayOnly) {
    await seedDemoEvents(host, port);
  }

  const lines = await rebuildAuditViewFromSubstrate(host, port);
  if (lines.length === 0) {
    throw new Error("audit-loop: substrate audit view is empty");
  }
  printStream(lines);
  console.log("audit-loop: ok");
}

main().catch((error) => {
  console.error(`audit-loop: ${(error as Error).message}`);
  process.exit(1);
});

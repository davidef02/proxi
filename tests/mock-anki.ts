import * as http from "node:http";

export interface MockCard {
  cardId?: number;
  note?: number;
  deckName: string;
  tags: string[];
  interval: number;
  lapses: number;
  reviews: { id: number; ease: number; ivl?: number }[];
}

export class MockAnki {
  cards: MockCard[] = [];
  requests: { action: string; version?: number; params?: any }[] = [];
  addedNotes: any[] = [];
  createdDecks: string[] = [];
  port: number = 0;
  shouldTimeout: boolean = false;
  responseError: string | null = null;
  /** Note types the mock Anki profile has; null = answer "Unsupported action" */
  models: Record<string, string[]> | null = null;
  deckStats: Record<string, any> | null = null;
  deckNamesAndIds: Record<string, number> | null = null;

  private server: http.Server | null = null;
  private nextCardId = 1001;
  private nextNoteId = 2001;

  reset(): void {
    this.requests = [];
    this.addedNotes = [];
    this.createdDecks = [];
    this.shouldTimeout = false;
    this.responseError = null;
    this.cards = [];
    this.models = null;
    this.deckStats = null;
    this.deckNamesAndIds = null;
  }

  private assignDefaults(): void {
    for (const card of this.cards) {
      if (card.cardId === undefined) {
        card.cardId = this.nextCardId++;
      }
      if (card.note === undefined) {
        card.note = this.nextNoteId++;
      }
    }
  }

  async start(): Promise<string> {
    return new Promise((resolve, reject) => {
      const server = http.createServer((req, res) => {
        if (this.shouldTimeout) {
          // Intentionally do not respond to trigger client timeout
          return;
        }

        let body = "";
        req.on("data", (chunk) => {
          body += chunk;
        });

        req.on("end", () => {
          try {
            const payload = body ? JSON.parse(body) : {};
            const action = payload.action;
            const params = payload.params;
            const version = payload.version;
            this.requests.push({ action, version, params });
            this.assignDefaults();

            res.writeHead(200, { "Content-Type": "application/json; charset=utf-8" });

            if (this.responseError) {
              res.end(JSON.stringify({ result: null, error: this.responseError }));
              return;
            }

            if (action === "version") {
              res.end(JSON.stringify({ result: 6, error: null }));
            } else if (action === "findCards") {
              const query = typeof params?.query === "string" ? params.query : "";
              const match = query.match(/^deck:"((?:[^"\\]|\\.)*)"$/);
              if (!match) {
                res.end(JSON.stringify({ result: [], error: null }));
                return;
              }
              const deckName = match[1].replace(/\\"/g, '"');
              const ids = this.cards
                .filter((c) => c.deckName === deckName || c.deckName.startsWith(deckName + "::"))
                .map((c) => c.cardId!);
              res.end(JSON.stringify({ result: ids, error: null }));
            } else if (action === "cardsInfo") {
              const requested = Array.isArray(params?.cards) ? params.cards : [];
              const result: any[] = [];
              for (const id of requested) {
                const card = this.cards.find((c) => c.cardId === id);
                if (card) {
                  result.push({
                    cardId: card.cardId,
                    note: card.note,
                    deckName: card.deckName,
                    interval: card.interval,
                    lapses: card.lapses,
                    factor: 2500,
                  });
                }
              }
              res.end(JSON.stringify({ result, error: null }));
            } else if (action === "notesInfo") {
              const requested = Array.isArray(params?.notes) ? params.notes : [];
              const result: any[] = [];
              for (const noteId of requested) {
                const card = this.cards.find((c) => c.note === noteId);
                if (card) {
                  result.push({
                    noteId,
                    tags: card.tags,
                  });
                }
              }
              res.end(JSON.stringify({ result, error: null }));
            } else if (action === "getReviewsOfCards") {
              const requested = Array.isArray(params?.cards) ? params.cards : [];
              const result: Record<string, any[]> = {};
              for (const cardId of requested) {
                const card = this.cards.find((c) => c.cardId === cardId);
                if (card) {
                  result[String(cardId)] = (card.reviews || []).map((r) => ({
                    id: r.id,
                    usn: 0,
                    ease: r.ease,
                    ivl: r.ivl ?? card.interval,
                    lastIvl: 0,
                    factor: 2500,
                    time: 1000,
                    type: 1,
                  }));
                } else {
                  result[String(cardId)] = [];
                }
              }
              res.end(JSON.stringify({ result, error: null }));
            } else if (action === "createDeck") {
              const deckName = params?.deck || "Default";
              this.createdDecks.push(deckName);
              res.end(JSON.stringify({ result: 1725128400000, error: null }));
            } else if (action === "addNotes") {
              const notes = Array.isArray(params?.notes) ? params.notes : [];
              this.addedNotes.push(...notes);
              const ids = notes.map(() => this.nextNoteId++);
              res.end(JSON.stringify({ result: ids, error: null }));
            } else if (action === "modelNames" && this.models) {
              res.end(JSON.stringify({ result: Object.keys(this.models), error: null }));
            } else if (action === "modelFieldNames" && this.models) {
              res.end(JSON.stringify({ result: this.models[params?.modelName] ?? null, error: null }));
            } else if (action === "deckNamesAndIds") {
              if (this.deckNamesAndIds) {
                res.end(JSON.stringify({ result: this.deckNamesAndIds, error: null }));
              } else {
                const deckNames = new Set<string>();
                for (const c of this.cards) {
                  if (c.deckName) deckNames.add(c.deckName);
                }
                for (const d of this.createdDecks) {
                  deckNames.add(d);
                }
                const result: Record<string, number> = {};
                let id = 1;
                for (const name of deckNames) {
                  result[name] = id++;
                }
                res.end(JSON.stringify({ result, error: null }));
              }
            } else if (action === "getDeckStats") {
              if (this.deckStats) {
                res.end(JSON.stringify({ result: this.deckStats, error: null }));
              } else {
                const result: Record<string, any> = {};
                const decks = Array.isArray(params?.decks) ? params.decks : [];
                for (const d of decks) {
                  result[String(d)] = { deck_id: 1, name: d, new_count: 0, learn_count: 0, review_count: 0, total_in_deck: 0 };
                }
                res.end(JSON.stringify({ result, error: null }));
              }
            } else {
              res.end(JSON.stringify({ result: null, error: `Unsupported action ${action}` }));
            }
          } catch (err: any) {
            res.writeHead(400, { "Content-Type": "application/json" });
            res.end(JSON.stringify({ result: null, error: err?.message || "Invalid JSON" }));
          }
        });
      });

      this.server = server;
      server.on("error", reject);
      server.listen(0, "127.0.0.1", () => {
        const addr = server.address();
        if (!addr || typeof addr === "string") {
          reject(new Error("Failed to get server address"));
          return;
        }
        this.port = addr.port;
        resolve(`http://127.0.0.1:${addr.port}`);
      });
    });
  }

  async stop(): Promise<void> {
    return new Promise((resolve, reject) => {
      if (!this.server) {
        resolve();
        return;
      }
      const s = this.server;
      this.server = null;
      if (typeof s.closeAllConnections === "function") {
        s.closeAllConnections();
      }
      s.close((err) => {
        if (err) reject(err);
        else resolve();
      });
    });
  }
}

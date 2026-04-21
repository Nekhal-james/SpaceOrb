/**
 * SpaceOrb V7.6 — WebSocket State Relay
 *
 * Connects to the Rust supervisor's 500ms state broadcast (Zenoh/WS)
 * and relays it to the Next.js dashboard clients.
 *
 * In production, this connects to the Zenoh WebSocket bridge.
 * In demo mode, it generates simulated state for exhibition.
 *
 * Reference: SPACEORB_CORE_SPEC.txt §5 (mission-control)
 */

import { NextRequest, NextResponse } from 'next/server';

const SUPERVISOR_WS_URL = process.env.SUPERVISOR_WS_URL || 'ws://localhost:9090/ws';

export async function GET(request: NextRequest) {
    // Return connection info for the WebSocket endpoint
    // The actual WebSocket connection is handled client-side to the supervisor directly,
    // or via a separate WebSocket server process.
    return NextResponse.json({
        service: 'mission-control-ws-relay',
        version: '7.6.0',
        supervisor_ws: SUPERVISOR_WS_URL,
        refresh_interval_ms: 500,
        description: 'Connect directly to the supervisor WebSocket for 500ms state updates',
        topics: {
            state: 'spaceorb/state',
            dtn_tx: 'spaceorb/dtn/tx',
            dtn_ack: 'spaceorb/dtn/ack',
        },
    });
}

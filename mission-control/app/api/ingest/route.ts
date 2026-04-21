/**
 * SpaceOrb V7.6 — Binary Ingest Proxy
 *
 * Streams uploaded files directly to the Pi's AI sandbox ingest port.
 * Acts as a stateless proxy between the ground station UI and the edge node.
 *
 * Reference: SPACEORB_CORE_SPEC.txt §5 (mission-control)
 */

import { NextRequest, NextResponse } from 'next/server';

const INGEST_TARGET = process.env.INGEST_TARGET_URL || 'http://localhost:5050/ingest';

export async function POST(request: NextRequest) {
    try {
        // Read the incoming request body as a stream
        const contentType = request.headers.get('content-type') || '';
        const body = await request.arrayBuffer();

        if (body.byteLength === 0) {
            return NextResponse.json(
                { error: 'Empty request body' },
                { status: 400 }
            );
        }

        // Forward to the Pi's ingest endpoint
        const upstreamResponse = await fetch(INGEST_TARGET, {
            method: 'POST',
            headers: {
                'Content-Type': contentType,
                'Content-Length': body.byteLength.toString(),
                'X-Forwarded-By': 'mission-control-proxy',
                'X-Request-Time': new Date().toISOString(),
            },
            body: body,
            signal: AbortSignal.timeout(30_000), // 30s timeout
        });

        const responseData = await upstreamResponse.json();

        return NextResponse.json(responseData, {
            status: upstreamResponse.status,
            headers: {
                'X-Upstream-Status': upstreamResponse.status.toString(),
                'X-Proxy': 'mission-control-v7.6',
            },
        });
    } catch (error) {
        const message = error instanceof Error ? error.message : 'Unknown proxy error';
        console.error('[Proxy] Ingest forwarding failed:', message);

        return NextResponse.json(
            {
                error: 'proxy_error',
                message: `Failed to forward to ingest endpoint: ${message}`,
                target: INGEST_TARGET,
            },
            { status: 502 }
        );
    }
}

export async function GET() {
    // Health check for the proxy route
    return NextResponse.json({
        service: 'mission-control-proxy',
        version: '7.6.0',
        target: INGEST_TARGET,
        status: 'ok',
    });
}

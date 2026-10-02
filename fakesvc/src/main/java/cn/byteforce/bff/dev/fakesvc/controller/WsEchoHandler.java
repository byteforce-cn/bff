package com.example.fakesvc.controller;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Component;
import org.springframework.web.socket.CloseStatus;
import org.springframework.web.socket.TextMessage;
import org.springframework.web.socket.WebSocketSession;
import org.springframework.web.socket.handler.TextWebSocketHandler;

/**
 * WebSocket Echo — 回显收到的每条消息。
 */
@Component
public class WsEchoHandler extends TextWebSocketHandler {

    private static final Logger log = LoggerFactory.getLogger(WsEchoHandler.class);

    @Override
    public void afterConnectionEstablished(WebSocketSession session) {
        log.info("WS Echo connected: {}", session.getId());
    }

    @Override
    protected void handleTextMessage(WebSocketSession session, TextMessage message) throws Exception {
        String payload = message.getPayload();
        log.debug("WS Echo received: {}", payload);
        session.sendMessage(new TextMessage(payload));
    }

    @Override
    public void afterConnectionClosed(WebSocketSession session, CloseStatus status) {
        log.info("WS Echo disconnected: {} status={}", session.getId(), status);
    }
}

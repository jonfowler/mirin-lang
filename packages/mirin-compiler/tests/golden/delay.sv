module reg2 (
    input  logic clk,
    input  logic a__valid,
    input  logic [7:0] a__payload,
    input  logic rstn,
    output logic result__valid,
    output logic [7:0] result__payload
);
    logic [7:0] payloadd;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            payloadd <= 0;
        end else begin
            payloadd <= a__payload;
        end
    end
    assign result__valid = a__valid;
    assign result__payload = payloadd;
endmodule

module double_delay_named (
    input  logic clk,
    input  logic rstn,
    output logic downstream__valid,
    output logic [7:0] downstream__payload,
    input  logic upstream__valid,
    input  logic [7:0] upstream__payload
);
    logic delay1__valid;
    logic [7:0] delay1__payload;
    reg2 reg2 (
        .clk(clk),
        .a__valid(upstream__valid),
        .a__payload(upstream__payload),
        .rstn(rstn),
        .result__valid(delay1__valid),
        .result__payload(delay1__payload)
    );
    logic delay2__valid;
    logic [7:0] delay2__payload;
    reg2 reg2_1 (
        .clk(clk),
        .a__valid(delay1__valid),
        .a__payload(delay1__payload),
        .rstn(rstn),
        .result__valid(delay2__valid),
        .result__payload(delay2__payload)
    );
    assign downstream__valid = delay2__valid;
    assign downstream__payload = delay2__payload;
endmodule

module test_out_binding_named (
    input  logic clk,
    input  logic rstn,
    input  logic upstream__valid,
    input  logic [7:0] upstream__payload,
    output logic downstream__valid,
    output logic [7:0] downstream__payload
);
    logic ds__valid;
    logic [7:0] ds__payload;
    double_delay_named double_delay_named (
        .clk(clk),
        .rstn(rstn),
        .downstream__valid(ds__valid),
        .downstream__payload(ds__payload),
        .upstream__valid(upstream__valid),
        .upstream__payload(upstream__payload)
    );
    double_delay_named double_delay_named_1 (
        .clk(clk),
        .rstn(rstn),
        .downstream__valid(downstream__valid),
        .downstream__payload(downstream__payload),
        .upstream__valid(ds__valid),
        .upstream__payload(ds__payload)
    );
endmodule

module double_delay_pos (
    input  logic clk,
    input  logic rstn,
    input  logic upstream__valid,
    input  logic [7:0] upstream__payload,
    output logic downstream__valid,
    output logic [7:0] downstream__payload
);
    logic delay1__valid;
    logic [7:0] delay1__payload;
    reg2 reg2 (
        .clk(clk),
        .a__valid(upstream__valid),
        .a__payload(upstream__payload),
        .rstn(rstn),
        .result__valid(delay1__valid),
        .result__payload(delay1__payload)
    );
    logic delay2__valid;
    logic [7:0] delay2__payload;
    reg2 reg2_1 (
        .clk(clk),
        .a__valid(delay1__valid),
        .a__payload(delay1__payload),
        .rstn(rstn),
        .result__valid(delay2__valid),
        .result__payload(delay2__payload)
    );
    assign downstream__valid = delay2__valid;
    assign downstream__payload = delay2__payload;
endmodule

module test_out_binding_pos (
    input  logic clk,
    input  logic rstn,
    input  logic upstream__valid,
    input  logic [7:0] upstream__payload,
    output logic downstream__valid,
    output logic [7:0] downstream__payload
);
    logic ds__valid;
    logic [7:0] ds__payload;
    double_delay_pos double_delay_pos (
        .clk(clk),
        .rstn(rstn),
        .upstream__valid(upstream__valid),
        .upstream__payload(upstream__payload),
        .downstream__valid(ds__valid),
        .downstream__payload(ds__payload)
    );
    double_delay_pos double_delay_pos_1 (
        .clk(clk),
        .rstn(rstn),
        .upstream__valid(ds__valid),
        .upstream__payload(ds__payload),
        .downstream__valid(downstream__valid),
        .downstream__payload(downstream__payload)
    );
endmodule

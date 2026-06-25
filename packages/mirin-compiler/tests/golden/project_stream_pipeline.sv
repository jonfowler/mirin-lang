module top (
    input  logic clk,
    input  logic rstn,
    input  logic upstream__valid,
    input  logic [7:0] upstream__data,
    output logic upstream__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready,
    output logic [8:0] result
);
    logic mid__valid;
    logic [7:0] mid__data;
    logic mid__ready;
    gain gain (
        .clk(clk),
        .upstream__valid(upstream__valid),
        .upstream__data(upstream__data),
        .upstream__ready(upstream__ready),
        .downstream__valid(mid__valid),
        .downstream__data(mid__data),
        .downstream__ready(mid__ready)
    );
    offset offset (
        .clk(clk),
        .upstream__valid(mid__valid),
        .upstream__data(mid__data),
        .upstream__ready(mid__ready),
        .downstream__valid(downstream__valid),
        .downstream__data(downstream__data),
        .downstream__ready(downstream__ready)
    );
    logic [8:0] count;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            count <= 0;
        end else begin
            count <= (count + 1);
        end
    end
    assign result = count;
endmodule

module gain (
    input  logic clk,
    input  logic upstream__valid,
    input  logic [7:0] upstream__data,
    output logic upstream__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready
);
    assign downstream__valid = upstream__valid;
    logic [7:0] __call_0;
    double double (
        .x(upstream__data),
        .result(__call_0)
    );
    assign downstream__data = __call_0;
    assign upstream__ready = downstream__ready;
endmodule

module offset (
    input  logic clk,
    input  logic upstream__valid,
    input  logic [7:0] upstream__data,
    output logic upstream__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready
);
    assign downstream__valid = upstream__valid;
    logic [7:0] __call_0;
    logic [7:0] __call_1;
    double double (
        .x(upstream__data),
        .result(__call_1)
    );
    add_one add_one (
        .x(__call_1),
        .result(__call_0)
    );
    assign downstream__data = __call_0;
    assign upstream__ready = downstream__ready;
endmodule

module double (
    input  logic [7:0] x,
    output logic [7:0] result
);
    assign result = (x + x);
endmodule

module add_one (
    input  logic [7:0] x,
    output logic [7:0] result
);
    assign result = (x + 1);
endmodule

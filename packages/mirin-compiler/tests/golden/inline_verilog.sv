module ff_en (
    input  logic clk,
    input  logic en,
    input  logic [7:0] d,
    output logic [7:0] q
);
    // A plain enabled flop, written by hand.
    always_ff @(posedge clk) begin
        if (en) q <= $unsigned(d);
    end
    initial begin
        assert ((8 * 2) == 16);
    end
endmodule

module top (
    input  logic clk,
    input  logic en,
    input  logic [7:0] d,
    output logic [7:0] result
);
    logic [7:0] q;
    ff_en ff_en (
        .clk(clk),
        .en(en),
        .d(d),
        .q(q)
    );
    assign result = q;
endmodule

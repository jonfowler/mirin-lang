module lits (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] a;
    assign a = (1 + x);
    logic [7:0] r;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            r <= 8'hFF;
        end else begin
            r <= a;
        end
    end
    logic [7:0] w;
    assign w = r;
    assign result = w;
endmodule

module fill #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [n-1:0] result
);
    assign result = (x + 200);
    initial begin
        assert (200 < (1 << n));
    end
endmodule

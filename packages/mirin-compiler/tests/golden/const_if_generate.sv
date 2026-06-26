module ground #(parameter int W) (
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    if ((W == 8)) begin : __block_0__g
        assign __block_0 = a;
    end else begin : __block_0__g
        assign __block_0 = b;
    end
    assign result = __block_0;
endmodule
